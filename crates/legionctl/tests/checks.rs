//! Project checks (design 23, E2) end to end: the real operator and the CLI on fixture repositories in a test home.
//! `detect` reads the language files, the checkpoint and merge gates run the repository's own commands at the tree
//! they commit or merge, each run keeps its log and profile in the dossier home, and a missing kind is a finding.

mod common;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::Operator;
use common::scratch::Scratch;
use serde_json::{Value, json};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn text(out: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

/// A test home with git's identity, workspaces under `worktrees`, and the real operator on it.
struct Lab {
    op: Operator,
    home: PathBuf,
}

impl Lab {
    fn new(tag: &str) -> Lab {
        // outside /tmp (private in a session sandbox) and outside any repository, like a real home
        let home = Scratch::new(format!(
            "/var/tmp/legionctl-checks-{tag}-{}",
            std::process::id()
        ));
        write(
            &home.join(".gitconfig"),
            "[user]\n  name = Test\n  email = test@example.com\n[init]\n  defaultBranch = main\n",
        );
        write(
            &home.join(".config/legionctl/workspace.jsonc"),
            &format!(
                r#"{{ "workspace": {{ "root": "{}" }} }}"#,
                home.join("worktrees").display()
            ),
        );
        std::fs::create_dir_all(home.join("root")).unwrap();
        let bin = Path::new(env!("CARGO_BIN_EXE_legionctl")).parent().unwrap();
        let path = std::env::join_paths(
            std::iter::once(bin.to_path_buf())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let mut op = Operator::start(
            tag,
            &[
                ("LEGIONCTL_SYSROOT", home.join("root").into()),
                ("HOME", OsString::from(&*home)),
                ("PATH", path),
                (
                    "LEGIONCTL_PLUGIN",
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../integrations/agent-plugin")
                        .into(),
                ),
            ],
        );
        let path = home.to_path_buf();
        op.held.push(home);
        Lab { op, home: path }
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("HOME", &self.home)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", text(&out).1);
        text(&out).0
    }

    /// A repository `name` with these files in its first commit on main.
    fn repo(&self, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let repo = self.home.join(name);
        for (file, content) in files {
            write(&repo.join(file), content);
        }
        self.git(&self.home, &["init", "--quiet", repo.to_str().unwrap()]);
        self.git(&repo, &["add", "."]);
        self.git(&repo, &["commit", "--quiet", "-m", "start"]);
        repo
    }

    /// `legionctl` in `cwd` as agent session `s1`.
    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        self.op.run(
            cwd,
            &[
                ("HOME", self.home.to_str().unwrap()),
                ("LEGIONCTL_SESSION", "s1"),
            ],
            args,
            "",
        )
    }

    /// stdout, also of a command that fails for its findings (`dossier check`, `doctor`).
    fn findings(&self, cwd: &Path, args: &[&str]) -> String {
        text(&self.run(cwd, args)).0
    }

    fn ok(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.run(cwd, args);
        assert!(out.status.success(), "{args:?}: {}", text(&out).1);
        text(&out).0
    }

    /// Bind session s1 to `dossier` in `repo` and open its workspace there.
    fn workspace(&self, repo: &Path, dossier: &str) -> PathBuf {
        self.ok(repo, &["dossier", "use", dossier]);
        let opened: Value =
            serde_json::from_str(&self.ok(repo, &["workspace", "open", "--json"])).unwrap();
        PathBuf::from(opened["path"].as_str().unwrap())
    }

    /// What the agent of session s1 reads at session start in `cwd`.
    fn session_start(&self, cwd: &Path) -> String {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude-code-hooks/session-start.json");
        let mut payload: Value =
            serde_json::from_str(&std::fs::read_to_string(fixture).unwrap()).unwrap();
        payload["session_id"] = json!("s1");
        payload["cwd"] = json!(cwd);
        let out = self
            .op
            .run(cwd, &[], &["hook", "session-start"], &payload.to_string());
        text(&out).0
    }

    fn count(&self, file: &str) -> usize {
        std::fs::read_to_string(self.home.join(file))
            .unwrap_or_default()
            .lines()
            .count()
    }
}

/// The run id in a `code checks run` line: `lint: exit 0 in 3 ms (run 7-lint; log ...)`.
fn run_id(line: &str) -> String {
    line.split("(run ")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

/// The proposal without its comment lines, as JSON.
fn proposal(lines: &str) -> Value {
    let json: String = lines
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}: {lines}"))
}

/// A repository of one language: its name, its files, the tools line and the proposed checks (name, kind, run).
type LanguageCase = (
    &'static str,
    &'static [(&'static str, &'static str)],
    &'static str,
    &'static [(&'static str, &'static str, &'static str)],
);

const LANGUAGE_CASES: [LanguageCase; 3] = [
    (
        "rust",
        &[(
            "Cargo.toml",
            "[package]\nname = \"fx\"\nversion = \"0.1.0\"\n",
        )],
        "toolchain rustup, lsp rust-analyzer, formatter rustfmt, linter clippy, analyzers cargo-deny",
        &[
            ("build", "build", "cargo build --workspace --locked"),
            ("format", "format", "cargo fmt --all --check"),
            (
                "lint",
                "lint",
                "cargo clippy --workspace --all-targets --locked -- -D warnings",
            ),
            (
                "test",
                "test",
                "cargo test --workspace --locked --no-fail-fast --message-format=json",
            ),
        ],
    ),
    (
        "node",
        &[
            (
                "package.json",
                r#"{"name": "fx", "scripts": {"build": "bun build index.ts"}}"#,
            ),
            ("bun.lock", "{}\n"),
        ],
        "toolchain bun, lsp typescript-language-server, formatter biome, linter biome",
        &[
            ("build", "build", "bun run build"),
            ("format", "format", "bunx biome format ."),
            ("lint", "lint", "bunx biome lint ."),
            ("test", "test", "bun test"),
            ("types", "types", "bunx tsc --noEmit"),
        ],
    ),
    (
        "python",
        &[("pyproject.toml", "[project]\nname = \"fx\"\n")],
        "toolchain uv, lsp ty server, formatter ruff, linter ruff, analyzers ty",
        &[
            ("build", "build", "uv build"),
            ("format", "format", "uvx ruff format --check ."),
            ("lint", "lint", "uvx ruff check ."),
            ("test", "test", "uv run pytest"),
            ("types", "types", "uvx ty check"),
        ],
    ),
];

/// `code checks detect` proposes each language's checks and names its tools: Rust (rustup, clippy), Node (bun, Biome)
/// and Python (uv, ruff, ty) from the shipped language files; a repository with two languages gets both sets with the
/// language in each name; one with no known manifest is refused with the manifests named. Detect writes nothing, and
/// its proposal, written as `.legionctl/checks.jsonc`, is the repository's checks (comments and all).
#[test]
fn detect_proposes_the_checks_of_each_language_from_the_language_files() {
    let lab = Lab::new("detect");
    let mut rust_text = String::new();
    for (name, files, tools, expected) in LANGUAGE_CASES {
        let repo = lab.repo(name, files);
        // from anywhere, with the repository named
        let proposed = lab
            .op
            .ok(&["code", "checks", "detect", "--repo", repo.to_str().unwrap()]);
        assert!(proposed.contains(tools), "{proposed}");
        let rows: Vec<(String, String, String)> = proposal(&proposed)["checks"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(n, c)| (n.clone(), c["kind"].to_string(), c["run"].to_string()))
            .collect();
        let expected: Vec<(String, String, String)> = expected
            .iter()
            .map(|(n, k, r)| ((*n).into(), json!(k).to_string(), json!(r).to_string()))
            .collect();
        assert_eq!(rows, expected, "{name}");
        if name == "rust" {
            assert_eq!(
                proposal(&proposed)["checks"]["test"]["runner"],
                json!({"format": "libtest"})
            );
            rust_text = proposed;
        }
    }

    two_languages_an_unknown_one_and_the_written_proposal(&lab, &rust_text);
}

/// Two languages give both sets, each name with its language; no known manifest is refused with the manifests named;
/// detect wrote nothing, and its proposal written as the repository's file is what the repository checks with.
fn two_languages_an_unknown_one_and_the_written_proposal(lab: &Lab, rust_text: &str) {
    let both = lab.repo(
        "both",
        &[("Cargo.toml", "[workspace]\n"), ("package.json", "{}\n")],
    );
    let names: Vec<String> = proposal(&lab.ok(&both, &["code", "checks", "detect"]))["checks"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        names,
        [
            "node-build",
            "node-format",
            "node-lint",
            "node-test",
            "node-types",
            "rust-build",
            "rust-format",
            "rust-lint",
            "rust-test"
        ]
    );

    let unknown = lab.repo("unknown", &[("README.md", "words\n")]);
    let refused = lab.run(&unknown, &["code", "checks", "detect"]);
    assert!(!refused.status.success());
    assert!(
        text(&refused)
            .1
            .contains("has none of package.json, pyproject.toml, Cargo.toml"),
        "{}",
        text(&refused).1
    );

    // detect wrote nothing; its proposal as the repository's file is what the repository checks with
    let rust = lab.home.join("rust");
    assert!(!rust.join(".legionctl").exists());
    write(&rust.join(".legionctl/checks.jsonc"), rust_text);
    let listed = lab.ok(&rust, &["code", "checks", "list"]);
    assert!(
        listed.contains(&format!(
            "lint (lint, gate checkpoint): cargo clippy --workspace --all-targets --locked -- -D warnings [{}]; last \
             run: none",
            rust.join(".legionctl/checks.jsonc").display()
        )),
        "{listed}"
    );
}

/// The checkpoint gate (E2 rule 4): a failing `gate: checkpoint` check refuses the checkpoint with the check's exit
/// code, log path and output, and commits nothing; a passing run at the tree being committed is not run again; a new
/// tree runs it again. Each run keeps its log, profile and `profiles` files in the dossier home, and `keep_runs`
/// removes older folders but the one an EVIDENCE front matter names.
#[test]
fn a_checkpoint_runs_its_gate_once_per_tree_and_a_failing_check_refuses_with_its_output() {
    let lab = Lab::new("ckpt");
    let repo = lab.repo(
        "repo",
        &[
            (".gitignore", "/agentWorkspaces\n/lint-report.txt\n"),
            (
                "lint.sh",
                "echo run >> \"$HOME/lint-runs\"\necho report > lint-report.txt\n\
                 if grep -n TODO *.txt; then echo 'lint: TODO left'; exit 3; fi\n",
            ),
            (
                ".legionctl/checks.jsonc",
                r#"// one gate check
{ "checks": { "keep_runs": 1,
  "lint": { "kind": "lint", "run": "sh lint.sh", "gate": "checkpoint", "profiles": ["lint-report.txt"] } } }"#,
            ),
        ],
    );
    let ws = lab.workspace(&repo, "ckpt");
    let commits = || lab.git(&ws, &["rev-list", "--count", "HEAD"]);

    write(&ws.join("notes.txt"), "TODO: finish\n");
    let refused = lab.run(&ws, &["workspace", "checkpoint", "add notes"]);
    let (_, err) = text(&refused);
    assert!(!refused.status.success());
    let runs = lab.home.join("agentWorkspaces/ckpt/.state/runs");
    assert!(
        err.contains(
            "checkpoint refused, nothing committed: check lint (lint) failed with exit 3; log "
        ) && err.contains(&runs.display().to_string())
            && err.contains("notes.txt:1:TODO: finish\nlint: TODO left"),
        "{err}"
    );
    assert_eq!(commits(), "1", "nothing committed");
    assert_eq!(lab.count("lint-runs"), 1);

    // a passing run at this tree is the evidence: the checkpoint does not run it again
    write(&ws.join("notes.txt"), "done\n");
    let ran = lab.ok(&ws, &["code", "checks", "run", "lint"]);
    let passed = run_id(&ran);
    assert!(ran.starts_with("lint: exit 0 in "), "{ran}");
    let out = lab.ok(&ws, &["workspace", "checkpoint", "add notes"]);
    assert!(
        out.contains(&format!("check lint: passed at this tree (run {passed})")),
        "{out}"
    );
    assert!(out.ends_with("add notes"), "{out}");
    assert_eq!(lab.count("lint-runs"), 2, "not run again");
    assert_eq!(commits(), "2");

    // the run's home: log, profile and the profiles file; show gives them
    let folder = runs.join(&passed);
    let profile: Value =
        serde_json::from_str(&std::fs::read_to_string(folder.join("profile.json")).unwrap())
            .unwrap();
    assert!(
        profile["wall_ms"].is_u64() && profile["max_rss_kib"].as_u64() > Some(0),
        "{profile}"
    );
    assert_eq!(
        std::fs::read_to_string(folder.join("lint-report.txt")).unwrap(),
        "report\n"
    );
    let shown = lab.ok(&ws, &["code", "checks", "show", &passed]);
    assert!(
        shown.contains(&format!("run {passed}: check lint (lint), exit 0"))
            && shown.contains("profile: wall ")
            && shown.contains(&format!("folder: {}", folder.display())),
        "{shown}"
    );

    // keep_runs 1: the failed run's folder went; an EVIDENCE that names a run keeps it past the limit
    assert_eq!(
        std::fs::read_dir(&runs).unwrap().count(),
        1,
        "only {passed}"
    );
    write(
        &lab.home.join("agentWorkspaces/ckpt/EVIDENCE/01-lint.md"),
        &format!("---\nid: EVIDENCE-01\nruns: [{passed}]\n---\nlint passed\n"),
    );
    write(&ws.join("more.txt"), "more\n");
    let out = lab.ok(&ws, &["workspace", "checkpoint", "add more"]);
    assert!(
        out.contains("check lint: passed (run "),
        "a new tree runs the check: {out}"
    );
    assert_eq!(lab.count("lint-runs"), 3);
    let mut kept: Vec<String> = std::fs::read_dir(&runs)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    kept.sort();
    assert_eq!(kept.len(), 2, "{kept:?}");
    assert!(kept.contains(&passed), "the EVIDENCE run stays: {kept:?}");
}

/// The merge gate (E2 rule 4): `workspace merge` needs passing `gate: merge` checks at the rebased head, not at the
/// branch before the rebase. A failure refuses the merge with the check's output and main stays; when main is fixed,
/// the merge runs the checks again and fast-forwards main.
#[test]
fn a_merge_refuses_without_passing_build_and_test_runs_at_the_rebased_head() {
    let lab = Lab::new("merge");
    let repo = lab.repo(
        "repo",
        &[
            (".gitignore", "/agentWorkspaces\n"),
            (
                "build.sh",
                "if [ -e broken ]; then echo 'build: broken is in the tree'; exit 2; fi\n",
            ),
            (
                ".legionctl/checks.jsonc",
                r#"{ "checks": {
  "build": { "kind": "build", "run": "sh build.sh", "gate": "merge" },
  "unit": { "kind": "test", "run": "echo run >> \"$HOME/unit-runs\"", "gate": "merge" } } }"#,
            ),
        ],
    );
    let ws = lab.workspace(&repo, "merge");
    write(&ws.join("feature.txt"), "feature\n");
    lab.ok(&ws, &["workspace", "checkpoint", "add feature"]);
    // both pass at the branch's own tree
    let ran = lab.ok(&ws, &["code", "checks", "run"]);
    assert!(
        ran.contains("build: exit 0") && ran.contains("unit: exit 0"),
        "{ran}"
    );
    assert_eq!(lab.count("unit-runs"), 1);

    // main moves on with a change that breaks the build: the rebased head is a new tree
    write(&repo.join("broken"), "x\n");
    lab.git(&repo, &["add", "broken"]);
    lab.git(&repo, &["commit", "--quiet", "-m", "break"]);
    let refused = lab.run(&ws, &["workspace", "merge"]);
    let (_, err) = text(&refused);
    assert!(!refused.status.success());
    assert!(
        err.contains(
            "merge refused at the rebased head of legionctl/merge (the branch stays rebased onto main): check build \
             (build) failed with exit 2"
        ) && err.contains("build: broken is in the tree"),
        "{err}"
    );
    assert_eq!(lab.git(&repo, &["log", "-1", "--format=%s"]), "break");
    assert_eq!(
        lab.count("unit-runs"),
        1,
        "the gate stops at the first failure"
    );

    // the fix is a tree that no run has seen (removing the file alone would give back the branch's checked tree)
    lab.git(&repo, &["rm", "--quiet", "broken"]);
    write(&repo.join("fixed.txt"), "fixed\n");
    lab.git(&repo, &["add", "fixed.txt"]);
    lab.git(&repo, &["commit", "--quiet", "-m", "unbreak"]);
    let merged = lab.ok(&ws, &["workspace", "merge"]);
    assert!(
        merged.contains("check build: passed (run ") && merged.contains("check unit: passed (run "),
        "{merged}"
    );
    assert_eq!(lab.git(&repo, &["log", "-1", "--format=%s"]), "add feature");
    assert_eq!(lab.count("unit-runs"), 2);
}

/// The merge gate tests only what the merge ships (R3-2, R3-7): an untracked file that makes the test pass refuses
/// the merge (fast-forward ships the commits without it, and main would not pass), and a branch that rewrites its
/// own gate in `.legionctl/` is refused for the user to merge. main stays at its commit each time.
#[test]
fn a_merge_gate_refuses_an_untracked_file_and_a_branch_that_sets_its_own_gate() {
    let lab = Lab::new("ships");
    let repo = lab.repo(
        "repo",
        &[
            (".gitignore", "/agentWorkspaces\n"),
            (
                ".legionctl/checks.jsonc",
                r#"{ "checks": { "unit": { "kind": "test", "run": "test -e helper.txt", "gate": "merge" } } }"#,
            ),
        ],
    );
    let ws = lab.workspace(&repo, "ships");
    write(&ws.join("feature.txt"), "uses helper.txt\n");
    lab.ok(&ws, &["workspace", "checkpoint", "add feature"]);
    write(&ws.join("helper.txt"), "never committed\n");
    let refused = lab.run(&ws, &["workspace", "merge"]);
    assert!(
        !refused.status.success() && text(&refused).1.contains("untracked files"),
        "{refused:?}"
    );
    assert_eq!(lab.git(&repo, &["log", "-1", "--format=%s"]), "start");

    std::fs::remove_file(ws.join("helper.txt")).unwrap();
    write(
        &ws.join(".legionctl/checks.jsonc"),
        r#"{ "checks": { "unit": { "kind": "test", "run": "true", "gate": "merge" } } }"#,
    );
    lab.ok(&ws, &["workspace", "checkpoint", "drop the gate"]);
    let refused = lab.run(&ws, &["workspace", "merge"]);
    assert!(
        !refused.status.success()
            && text(&refused)
                .1
                .contains("changes .legionctl/, which sets its own gates: the user merges it"),
        "{refused:?}"
    );
    assert_eq!(lab.git(&repo, &["log", "-1", "--format=%s"]), "start");
}

/// A gate passes only on evidence (R3-3, R3-9): a syntax error in the repository's `checks.jsonc` refuses the
/// checkpoint instead of dropping every gate, and a gate check that changes the files while it runs (a fixing
/// formatter) refuses, since its pass is for other content than the commit. Nothing is committed either time.
#[test]
fn a_broken_checks_file_and_a_check_that_changes_the_files_refuse_the_checkpoint() {
    let lab = Lab::new("closed");
    let repo = lab.repo(
        "repo",
        &[
            (".gitignore", "/agentWorkspaces\n"),
            (
                ".legionctl/checks.jsonc",
                r#"{ "checks": { "lint": { "kind": "lint", "run": "false", "gate": "checkpoint" } } }"#,
            ),
        ],
    );
    let ws = lab.workspace(&repo, "closed");
    let commits = || lab.git(&ws, &["rev-list", "--count", "HEAD"]);
    // a missing comma
    write(
        &ws.join(".legionctl/checks.jsonc"),
        r#"{ "checks": { "lint": { "kind": "lint" "run": "false", "gate": "checkpoint" } } }"#,
    );
    let refused = lab.run(&ws, &["workspace", "checkpoint", "broken gate"]);
    let err = text(&refused).1;
    assert!(
        !refused.status.success()
            && err.contains("the repository's checks do not resolve, so no gate can pass")
            && err.contains("checks.jsonc"),
        "{err}"
    );
    assert_eq!(commits(), "1");

    write(
        &ws.join(".legionctl/checks.jsonc"),
        r#"{ "checks": { "fmt": { "kind": "format", "run": "echo formatted >> code.txt", "gate": "checkpoint" } } }"#,
    );
    write(&ws.join("code.txt"), "code\n");
    let refused = lab.run(&ws, &["workspace", "checkpoint", "formatted"]);
    let err = text(&refused).1;
    assert!(
        !refused.status.success()
            && err.contains("check fmt (format) changed the files of the workspace while it ran"),
        "{err}"
    );
    assert_eq!(commits(), "1");
}

/// A repository without a required kind (`checks.required`) is a finding in `doctor` and `dossier check`, and the
/// agent is told at session start; a check whose command is not found (exit 127) is a "check broken" finding. The
/// checks are the branch's own: once the workspace's file has the kinds, the nudge and the finding go.
#[test]
fn a_missing_check_kind_is_a_finding_and_a_session_start_nudge() {
    let lab = Lab::new("missing");
    let repo = lab.repo(
        "repo",
        &[
            (".gitignore", "/agentWorkspaces\n"),
            (
                ".legionctl/checks.jsonc",
                r#"{ "checks": {
  "format": { "kind": "format", "run": "true" },
  "lint": { "kind": "lint", "run": "no-such-linter --check" } } }"#,
            ),
        ],
    );
    let ws = lab.workspace(&repo, "missing");
    let nudge = "legionctl: this repository has no checks of kind build, test: run `legionctl code checks detect` \
                 and write .legionctl/checks.jsonc.";
    let start = lab.session_start(&ws);
    assert!(start.contains(nudge), "{start}");
    let finding = format!(
        "{}: this repository has no checks of kind build, test",
        ws.display()
    );
    assert!(lab.findings(&ws, &["dossier", "check"]).contains(&finding));
    // doctor reads the machine too: an empty one with a boot id
    write(
        &lab.home.join("root/proc/sys/kernel/random/boot_id"),
        "00000000-0000-0000-0000-000000000001\n",
    );
    let profile = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../profiles/lenovo-82wq-legion-pro-7-16irx8h.json");
    let doctor = || {
        text(
            &lab.op
                .cli(&["doctor", "--profile", profile.to_str().unwrap()]),
        )
        .0
    };
    assert!(doctor().contains(&finding));

    assert!(
        lab.ok(&ws, &["code", "checks", "run", "lint"])
            .starts_with("lint: exit 127")
    );
    let broken = format!(
        "{}: check lint broken: its command was not found (exit 127): no-such-linter --check",
        ws.display()
    );
    assert!(lab.findings(&ws, &["dossier", "check"]).contains(&broken));
    assert!(doctor().contains(&broken));

    write(
        &ws.join(".legionctl/checks.jsonc"),
        r#"{ "checks": {
  "format": { "kind": "format", "run": "true" }, "lint": { "kind": "lint", "run": "true" },
  "build": { "kind": "build", "run": "true" }, "test": { "kind": "test", "run": "true" } } }"#,
    );
    assert!(!lab.session_start(&ws).contains("no checks of kind"));
    let checked = lab.findings(&ws, &["dossier", "check"]);
    assert!(
        !checked.contains("no checks of kind") && !checked.contains("broken"),
        "{checked}"
    );
}
