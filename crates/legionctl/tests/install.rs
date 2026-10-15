//! The checked install (packaging/install): `legionctl-install`, which sudo runs with no password, against a git
//! origin in a folder and stand-ins for the GitHub API (curl), runuser, makepkg, pacman, systemctl, logger and rustup;
//! and the sudoers line through `visudo -c`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "common/scratch.rs"]
mod scratch;

use scratch::Scratch;

const WRAPPER: &str = "../../packaging/install/legionctl-install";
const SUDOERS: &str = "../../packaging/install/sudoers";
const PACKAGE: &str = "legionctl-1-1-x86_64.pkg.tar.zst";

struct Lab {
    dir: Scratch,
}

impl Lab {
    fn new() -> Lab {
        let dir = Scratch::new(
            std::env::temp_dir().join(format!("legionctl-install-{}", std::process::id())),
        );
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        // what tmpfiles.d makes in the real root
        std::fs::create_dir_all(dir.join("root/var/lib/legionctl-build")).unwrap();
        let d = dir.display();
        for (name, body) in [
            // only the build user: each tool that it runs logs AS
            (
                "runuser",
                "[ \"$1 $2 $3\" = \"-u legionctl-build --\" ] || exit 9; shift 3; AS=legionctl-build exec \"$@\"".to_string(),
            ),
            ("curl", format!("echo \"$AS $*\" >> {d}/curl.log; cat {d}/runs.json")),
            (
                "makepkg",
                format!(
                    "[ -f PKGBUILD ] || exit 1\n\
                     [ -n \"$BUILDDIR\" ] || {{ echo 'ERROR: You do not have write permission for the directory $BUILDDIR (/tmp/makepkg).' >&2; exit 1; }}\n\
                     if [ \"$1\" = --packagelist ]; then echo \"$PKGDEST/{PACKAGE}\"; exit; fi\n\
                     echo \"$AS $PWD BUILDDIR=$BUILDDIR PKGDEST=$PKGDEST\" >> {d}/makepkg.log\n\
                     mkdir -p \"$PKGDEST\" && git rev-parse HEAD > \"$PKGDEST/{PACKAGE}\""
                ),
            ),
            ("pacman", format!("echo \"$AS|$*|$(cat \"$3\")\" >> {d}/pacman.log")),
            ("systemctl", format!("echo \"$*\" >> {d}/systemctl.log")),
            ("logger", format!("echo \"$*\" >> {d}/journal.log")),
            ("rustup", format!("echo \"$AS $*\" >> {d}/rustup.log")),
            // with the file lie, git says that the checkout is at an other commit
            (
                "git",
                format!(
                    "if [ -e {d}/lie ] && [ \"$3\" = rev-parse ]; then echo 1111111111111111111111111111111111111111; exit; fi\n\
                     exec /usr/bin/git \"$@\""
                ),
            ),
        ] {
            let path = bin.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
        }
        git(&dir, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
        git(&dir, &["init", "-q", "-b", "main", "work"]);
        let work = dir.join("work");
        git(&work, &["remote", "add", "origin", "../origin.git"]);
        std::fs::create_dir_all(work.join("packaging/arch")).unwrap();
        // a package in the user's folder that the wrapper must never take
        std::fs::write(work.join("packaging/arch").join(PACKAGE), "decoy").unwrap();
        Lab { dir }
    }

    fn work(&self) -> PathBuf {
        self.dir.join("work")
    }

    /// One commit in the user's folder, pushed to `branch` of the origin; its SHA.
    fn push(&self, branch: &str, text: &str) -> String {
        let work = self.work();
        std::fs::write(work.join("packaging/arch/PKGBUILD"), text).unwrap();
        git(&work, &["add", "packaging/arch/PKGBUILD"]);
        git(&work, &["commit", "-qm", text]);
        git(&work, &["push", "-q", "origin", &format!("HEAD:{branch}")]);
        git(&work, &["rev-parse", "HEAD"])
    }

    /// The GitHub API's answer: the runs of `ci.yml` as (`head_sha`, `status`, `conclusion`).
    fn runs(&self, runs: &[(&str, &str, &str)]) {
        let runs: Vec<_> = runs
            .iter()
            .map(|(sha, status, conclusion)| {
                serde_json::json!({"head_sha": sha, "status": status, "conclusion": conclusion})
            })
            .collect();
        std::fs::write(
            self.dir.join("runs.json"),
            serde_json::json!({ "workflow_runs": runs }).to_string(),
        )
        .unwrap();
    }

    /// The wrapper as sudo starts it for alice, from her repository folder.
    fn install(&self, args: &[&str]) -> Output {
        Command::new(Path::new(env!("CARGO_MANIFEST_DIR")).join(WRAPPER))
            .args(args)
            .current_dir(self.work())
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.dir.join("bin").display()),
            )
            .env("SUDO_USER", "alice")
            .env("LEGIONCTL_INSTALL_ROOT", self.dir.join("root"))
            .env("LEGIONCTL_INSTALL_ORIGIN", self.dir.join("origin.git"))
            .output()
            .unwrap()
    }

    fn refused(&self, args: &[&str], reason: &str) {
        let out = self.install(args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "not refused: {stderr}");
        assert!(stderr.contains(reason), "no '{reason}' in: {stderr}");
        assert!(
            self.log("journal.log")
                .contains(&format!("refused: {reason}"))
        );
    }

    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join(name)).unwrap_or_default()
    }
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn the_install_wrapper_installs_only_the_package_built_from_main_after_its_ci_passed() {
    let lab = Lab::new();
    let first = lab.push("main", "one");
    let branch = lab.push("topic", "branch only");
    let home = lab.dir.join("root/var/lib/legionctl-build");
    let built = home.join("pkg").join(PACKAGE);

    // no input from the caller: an argument is refused before any step
    lab.runs(&[(&first, "completed", "success")]);
    lab.refused(&["--repo", "/tmp/x"], "this command takes no arguments");
    assert_eq!(lab.log("curl.log"), "");

    // the CI run of main's commit: failed, missing, or a passed run of an other commit
    lab.runs(&[(&first, "completed", "failure")]);
    lab.refused(&[], &format!("{first}: CI run completed failure"));
    lab.runs(&[]);
    lab.refused(&[], &format!("{first}: no CI run"));
    lab.runs(&[(&branch, "completed", "success")]);
    lab.refused(&[], &format!("{first}: no CI run"));
    assert!(
        lab.log("curl.log")
            .lines()
            .all(|l| l.starts_with("legionctl-build ")
                && l.contains(&format!(
                    "ci.yml/runs?head_sha={first}&branch=main&event=push"
                ))),
        "{}",
        lab.log("curl.log")
    );
    assert_eq!(lab.log("makepkg.log"), "");

    // passed: legionctl-build builds main's commit in its home, root installs exactly that file
    lab.runs(&[
        (&first, "completed", "failure"),
        (&first, "completed", "success"),
    ]);
    let out = lab.install(&[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        lab.log("makepkg.log"),
        format!(
            "legionctl-build {src}/packaging/arch BUILDDIR={home}/build PKGDEST={home}/pkg\n",
            src = home.join("src").display(),
            home = home.display()
        )
    );
    assert_eq!(
        lab.log("pacman.log"),
        format!("|-U --noconfirm {}|{first}\n", built.display())
    );
    assert_eq!(
        lab.log("systemctl.log"),
        "--user -M alice@ daemon-reload\n--user -M alice@ restart legionctl-operator.service\n"
    );
    assert_eq!(lab.log("rustup.log"), "legionctl-build default stable\n");

    // main moves: the next install fetches into the same clone and installs the new commit only
    let second = lab.push("main", "two");
    lab.runs(&[(&second, "completed", "success")]);
    assert!(lab.install(&[]).status.success());
    assert_eq!(
        lab.log("pacman.log").lines().last().unwrap(),
        format!("|-U --noconfirm {}|{second}", built.display())
    );

    // a checkout that does not end at main's commit is refused before the build
    std::fs::write(lab.dir.join("lie"), "").unwrap();
    lab.refused(
        &[],
        &format!("the checkout is at 1111111111111111111111111111111111111111, not {second}"),
    );
    assert_eq!(lab.log("pacman.log").lines().count(), 2);
    assert!(!lab.log("pacman.log").contains("decoy"));
}

#[test]
fn the_sudoers_line_parses() {
    let out = Command::new("visudo")
        .args(["-c", "-f"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join(SUDOERS))
        .output()
        .expect("visudo (package sudo) runs the check");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
