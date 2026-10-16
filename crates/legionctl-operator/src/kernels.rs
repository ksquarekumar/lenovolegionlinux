//! Kernels, todo-84 parts 3 and 4 (docs/design/08-kernel.md, "Kernels"): one bench suite on each installed kernel in
//! a series (a job that goes on after each restart), and a kernel package built from an installed `linux-cachyos`
//! kernel (a job that goes on from its last step).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use legionctl_api::{
    BenchIn, Changes, Ctx, Engine, KernelBench, KernelBenchIn, KernelBuildIn, KernelIn, Principal,
    Scenario,
};
use legionctl_core::db::{Db, EventFilter, Job};
use legionctl_machine::sysroot::SysRoot;
use legionctl_machine::{boot, kernel, load};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::engine::{Operator, bench_run_of, config};

/// The job of a bench series.
pub(crate) const SERIES: &str = "kernel.bench";
/// The job of a kernel build.
pub(crate) const BUILD: &str = "kernel.build";
/// The PKGBUILDs of the `linux-cachyos` kernels, one folder per kernel.
const REPO: &str = "https://github.com/CachyOS/linux-cachyos";
/// The PKGBUILD line that names the package; the build puts its own `_pkgsuffix` before it.
const PKGBASE: &str = "pkgbase=\"linux-$_pkgsuffix\"";
/// The build container: Arch with makepkg, and a user `build` with the caller's uid (it writes the packages into the
/// caller's folder) that may install the build dependencies (`makepkg -s`).
const IMAGE: &str = "legionctl-kernel-build";
const DOCKERFILE: &str = "FROM archlinux:base-devel\nARG UID=1000\n\
     RUN pacman -Syu --noconfirm --needed git && useradd -m -u \"$UID\" build && \
     echo 'build ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/build\nUSER build\n";
/// Added to the PKGBUILD with a fragment: its own `prepare()` first, then the fragment merged into its
/// configuration, which `olddefconfig` completes.
const MERGE: &str = r#"
# legionctl kernel build: the configuration fragment, merged after the PKGBUILD's own configuration
eval "_legionctl_$(declare -f prepare)"
prepare() {
    _legionctl_prepare
    cd "$srcdir/$_srcname"
    scripts/kconfig/merge_config.sh -m .config "$startdir/legionctl.config"
    make "${BUILD_FLAGS[@]}" olddefconfig
    make -s kernelrelease > version
}
"#;
/// How long a step of a series waits for an idle machine after login: 60 times 10 s.
const IDLE_CHECKS: u32 = 60;

/// A series as its job keeps it.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Series {
    scenario: Scenario,
    seconds: u32,
    /// Each kernel starts once, in this order.
    kernels: Vec<String>,
    /// The default kernel, which starts once more at the end.
    default: String,
    /// The kernel that the series started last: `kernels[at]`, or the default after the last one.
    at: usize,
    /// The bench runs so far (their event ids).
    runs: Vec<i64>,
}

impl Series {
    fn expected(&self) -> &str {
        self.kernels.get(self.at).unwrap_or(&self.default)
    }
}

/// A kernel build as its job keeps it.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Build {
    /// `linux-cachyos-<name>`
    package: String,
    /// The PKGBUILD's folder in [`REPO`] (`linux-cachyos-bore` for `linux-cachyos-bore-lto`).
    folder: String,
    /// The installed base's version (`7.2.9-2`), which pins the PKGBUILD.
    version: String,
    /// `_use_llvm_lto`: thin, full or none, as the base was built.
    lto: String,
    /// The configuration fragment.
    fragment: Option<String>,
    /// The clone, the image and the log.
    dir: PathBuf,
}

fn ctx() -> Ctx {
    Ctx {
        actor: "operator".into(),
        principal: Principal::User,
    }
}

/// `command`'s stdout, or why it failed.
fn run(command: &mut Command) -> Result<String> {
    let out = command
        .output()
        .with_context(|| format!("running {}", command.get_program().display()))?;
    anyhow::ensure!(
        out.status.success(),
        "{} failed: {}",
        command.get_program().display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git(repo: &Path) -> Command {
    let mut git = Command::new("git");
    git.arg("-C").arg(repo);
    git
}

/// The commit of `folder` whose `.SRCINFO` has the version `version` (`pkgver-pkgrel`), newest first.
fn pin(repo: &Path, folder: &str, version: &str) -> Result<String> {
    let srcinfo = format!("{folder}/.SRCINFO");
    for commit in run(git(repo).args(["log", "--format=%H", "--", &srcinfo]))?.lines() {
        let text = run(git(repo).args(["show", &format!("{commit}:{srcinfo}")]))?;
        let field = |key: &str| {
            text.lines()
                .find_map(|l| l.trim().strip_prefix(key)?.strip_prefix(" = "))
                .unwrap_or_default()
                .to_string()
        };
        if format!("{}-{}", field("pkgver"), field("pkgrel")) == version {
            return Ok(commit.to_string());
        }
    }
    bail!("no commit of {folder} in {REPO} has the version {version}")
}

/// The PKGBUILD names the package `package` and, with a fragment, merges it.
fn patch(dir: &Path, build: &Build) -> Result<()> {
    let file = dir.join("PKGBUILD");
    let text =
        std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    anyhow::ensure!(
        text.lines().any(|l| l == PKGBASE),
        "{} has no line {PKGBASE}: its naming changed, so legionctl cannot name the package",
        file.display()
    );
    let suffix = build.package.trim_start_matches("linux-");
    let mut text = text.replacen(PKGBASE, &format!("_pkgsuffix={suffix}\n{PKGBASE}"), 1);
    if let Some(fragment) = &build.fragment {
        std::fs::write(dir.join("legionctl.config"), fragment)?;
        text.push_str(MERGE);
    }
    std::fs::write(&file, text)?;
    Ok(())
}

/// The record of the packages in the PKGBUILD's folder: the version and the fragment they were built with.
const BUILT: &str = "legionctl.built";

/// What [`BUILT`] holds for `build`.
fn record(build: &Build) -> String {
    format!(
        "{}\n{}",
        build.version,
        build.fragment.as_deref().unwrap_or_default()
    )
}

/// The packages in `dir`.
fn packages(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(".pkg.tar.zst"))
        .collect()
}

/// The kernel package of `build` (not its headers) is in `dir`, of its version and its fragment.
fn built(dir: &Path, build: &Build) -> bool {
    let prefix = format!("{}-{}-", build.package, build.version);
    std::fs::read_to_string(dir.join(BUILT)).is_ok_and(|r| r == record(build))
        && packages(dir).iter().any(|p| {
            p.file_name()
                .is_some_and(|f| f.to_string_lossy().starts_with(&prefix))
        })
}

/// The plan of `kernel build`, refused before anything happens: a bad or installed name, a base that is not an
/// installed `linux-cachyos` kernel, a fragment with lines that are not kernel options.
fn plan(input: &KernelBuildIn) -> Result<(Build, Vec<String>)> {
    let root = SysRoot::from_env();
    let name = &input.name;
    anyhow::ensure!(
        !name.is_empty()
            && name.len() <= 32
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !name.starts_with('-')
            && !name.ends_with('-'),
        "the name must be 1-32 of a-z, 0-9 and '-' (not first or last): {name}"
    );
    let package = format!("linux-cachyos-{name}");
    anyhow::ensure!(
        !kernel::installed_packages(&root)?.contains(&package),
        "{package} is an installed kernel already: give another --name"
    );
    let base = &input.base;
    let release = kernel::release_of(&root, base)?
        .with_context(|| format!("no installed kernel {base} (see legionctl kernel list)"))?;
    let version = base
        .strip_prefix("linux-cachyos")
        .and_then(|_| release.strip_suffix(&base["linux".len()..]))
        .with_context(|| {
            format!("{base} ({release}) is not a CachyOS kernel: kernel build uses the CachyOS PKGBUILDs only")
        })?
        .to_string();
    let fragment = input
        .config_fragment
        .as_ref()
        .map(|p| -> Result<String> {
            let path = p.get()?;
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            for line in text.lines().map(str::trim) {
                anyhow::ensure!(
                    line.is_empty()
                        || line.starts_with('#')
                        || (line.starts_with("CONFIG_") && line.contains('=')),
                    "{}: '{line}' is not a kernel option (CONFIG_<option>=<value>, or # CONFIG_<option> is not set)",
                    path.display()
                );
            }
            Ok(text)
        })
        .transpose()?;
    let folder = base.strip_suffix("-lto").unwrap_or(base).to_string();
    let dir = legionctl_core::cache::default_path()
        .with_file_name("kernel-build")
        .join(&package);
    let lto = kernel::lto(&root, &release)?.with_context(|| {
        format!("no build/.config for {release}: install {base}-headers (the build takes its LTO mode from it)")
    })?;
    let lines = vec![
        format!(
            "clone {REPO} into {} and check out the commit whose {folder}/.SRCINFO is {version} (the installed {base})",
            dir.join("linux-cachyos").display()
        ),
        format!(
            "PKGBUILD: the package {package} (release {version}-cachyos-{name}){}",
            if fragment.is_some() {
                "; prepare() merges the fragment with scripts/kconfig/merge_config.sh, then make olddefconfig"
            } else {
                ""
            }
        ),
        format!(
            "makepkg -s in a container (image {IMAGE}, from archlinux:base-devel), _use_llvm_lto={lto}; the log in {}",
            dir.join("build.log").display()
        ),
        format!(
            "install {package} and {package}-headers through legionctl-system: pacman -U (polkit org.legionctl.boot)"
        ),
        format!(
            "then pacman's hooks: mkinitcpio makes /boot/initramfs-{package}.img, legionctl the signed image \
             legionctl-{package}.efi (the MOK key, the running command line), DKMS the nvidia and legionctl-wmi modules"
        ),
    ];
    Ok((
        Build {
            package,
            folder,
            version,
            lto: lto.into(),
            fragment,
            dir,
        },
        lines,
    ))
}

impl Operator {
    /// `kernel bench`: the newest run of the suite on each kernel; `--all` starts a series, `--cancel` stops it.
    pub(crate) fn bench_kernels(&self, ctx: &Ctx, input: &KernelBenchIn) -> Result<KernelBench> {
        let db = Db::open_default()?;
        let series = db
            .jobs_in("running")?
            .into_iter()
            .find(|j| j.kind == SERIES)
            .map(|j| -> Result<(i64, Series)> { Ok((j.id, serde_json::from_value(j.input)?)) })
            .transpose()?;
        let answer = |lines: Vec<String>, dry_run: bool| KernelBench {
            runs: Vec::new(),
            lines,
            dry_run,
        };
        if input.cancel {
            let (id, series) = series
                .filter(|(_, s)| s.scenario == input.scenario)
                .context("no kernel bench series of this suite runs")?;
            db.job_end(id, &Err("cancelled by the user".into()))?;
            return Ok(answer(
                vec![format!(
                    "series job {id} stopped before its bench on {}; the default kernel {} starts at the next start",
                    series.expected(),
                    series.default
                )],
                false,
            ));
        }
        if !input.all {
            let mut view = Self::kernel_runs(&db, input.scenario)?;
            view.lines.extend(series.map(|(id, s)| {
                format!(
                    "series job {id} runs: {} of {} kernels done, {} next",
                    s.at,
                    s.kernels.len(),
                    s.expected()
                )
            }));
            return Ok(view);
        }
        anyhow::ensure!((1..=600).contains(&input.seconds), "seconds must be 1-600");
        if let Some((id, _)) = series {
            bail!(
                "series job {id} runs already (legionctl kernel bench <suite> --cancel stops it)"
            );
        }
        let root = SysRoot::from_env();
        let state = boot::state(&root)?;
        let default = boot::default_kernel(&root)?.context(
            "legionctl's boot entry is not installed: install the boot takeover first (legionctl boot install)",
        )?;
        let mut kernels = state.kernels.clone();
        kernels.sort();
        // each kernel must start through the takeover: refuse before the first restart
        for name in &kernels {
            boot::try_plan(&state, name)?;
        }
        let suite = serde_json::to_value(input.scenario)?;
        let mut lines: Vec<String> = kernels
            .iter()
            .map(|k| {
                format!(
                    "start {k} once (kernel try: a restart), then bench {} {} s after login",
                    suite.as_str().unwrap_or_default(),
                    input.seconds
                )
            })
            .collect();
        lines.push(format!(
            "start {default} once more (the default kernel); the series ends"
        ));
        if input.dry_run {
            return Ok(answer(lines, true));
        }
        let series = Series {
            scenario: input.scenario,
            seconds: input.seconds,
            kernels,
            default,
            at: 0,
            runs: Vec::new(),
        };
        let id = db.job_begin(SERIES, &serde_json::to_value(&series)?)?;
        lines.push(format!(
            "series job {id}: legionctl jobs show {id}; legionctl kernel bench <suite> --cancel stops it"
        ));
        let first = KernelIn {
            name: series.expected().into(),
            dry_run: false,
        };
        if let Err(e) = self.kernel_try(ctx, first) {
            db.job_end(id, &Err(format!("{e:#}")))?;
            return Err(e);
        }
        Ok(answer(lines, false))
    }

    /// The newest run of `scenario` on each kernel.
    fn kernel_runs(db: &Db, scenario: Scenario) -> Result<KernelBench> {
        let filter = EventFilter {
            kinds: &["bench.result"],
            ..EventFilter::default()
        };
        let mut newest = BTreeMap::new();
        let mut after = 0;
        loop {
            let (rows, next) = db.events_page(&filter, after, 500)?;
            for row in rows {
                let run = bench_run_of(row)?;
                if run.result.scenario == scenario
                    && let Some(kernel) = run.result.kernel.clone()
                {
                    newest.insert(kernel, run);
                }
            }
            match next {
                Some(n) => after = n,
                None => break,
            }
        }
        Ok(KernelBench {
            runs: newest.into_values().collect(),
            lines: Vec::new(),
            dry_run: false,
        })
    }

    /// A series after a start (the operator starts at login): its kernel must be the one it asked for; then the
    /// suite and the next start, or the end. A failure ends the job.
    pub(crate) fn series_step(&self, db: &Db, job: &Job) -> Result<()> {
        let step = serde_json::from_value(job.input.clone())
            .map_err(anyhow::Error::from)
            .and_then(|series| self.series_go(db, job.id, series));
        if let Err(e) = step {
            db.job_end(job.id, &Err(format!("{e:#}")))?;
        }
        Ok(())
    }

    fn series_go(&self, db: &Db, id: i64, mut series: Series) -> Result<()> {
        let root = SysRoot::from_env();
        let expected = series.expected().to_string();
        let running = kernel::running_package(&root)?.unwrap_or_else(|| "an unknown kernel".into());
        anyhow::ensure!(
            running == expected,
            "{expected} did not start ({running} runs): the series stopped there; the default kernel {} starts at \
             the next start",
            series.default
        );
        if series.at == series.kernels.len() {
            return db.job_end(id, &Ok(json!({ "runs": series.runs })));
        }
        // after login the desktop starts its own work: the bench waits for an idle machine (at most 10 min)
        let limits = config(None)?.config.load;
        for _ in 0..IDLE_CHECKS {
            if load::read_load(&root, &limits)?.level == load::Level::Idle {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(10));
        }
        let run = self.bench_run(
            &ctx(),
            BenchIn {
                scenario: series.scenario,
                seconds: series.seconds,
                label: Some(format!("series {id}")),
            },
        )?;
        series.runs.push(run.id);
        series.at += 1;
        // false: cancelled during the bench
        if db.job_step(id, &serde_json::to_value(&series)?)? {
            self.kernel_try(
                &ctx(),
                KernelIn {
                    name: series.expected().into(),
                    dry_run: false,
                },
            )?;
        }
        Ok(())
    }

    /// `kernel build`: the plan, or the build as a job.
    pub(crate) fn build_kernel(&self, input: &KernelBuildIn) -> Result<Changes> {
        let (build, mut lines) = plan(input)?;
        if input.dry_run {
            return Ok(Changes {
                lines,
                dry_run: true,
            });
        }
        let db = Db::open_default()?;
        let id = db.job_begin(BUILD, &serde_json::to_value(&build)?)?;
        let out = self.run_build(id, &build);
        db.job_end(
            id,
            &out.as_ref().map(|l| json!(l)).map_err(|e| format!("{e:#}")),
        )?;
        lines.extend(out?);
        Ok(Changes {
            lines,
            dry_run: false,
        })
    }

    /// The build from its last step: clone (once), pin, name, makepkg (unless the package is there), install. Each
    /// step is a `kernel.build.progress` event.
    pub(crate) fn run_build(&self, job: i64, build: &Build) -> Result<Vec<String>> {
        let db = Db::open_default()?;
        let progress = |step: &str| {
            db.record(
                "operator",
                "kernel.build.progress",
                &json!({ "job": job, "package": build.package, "step": step }),
            )
        };
        std::fs::create_dir_all(&build.dir)?;
        let repo = build.dir.join("linux-cachyos");
        if !repo.join(".git").exists() {
            progress("clone")?;
            run(Command::new("git")
                .args(["clone", "--quiet", REPO])
                .arg(&repo))?;
        }
        progress("pin")?;
        let commit = pin(&repo, &build.folder, &build.version)?;
        run(git(&repo).args(["checkout", "--quiet", "--force", &commit]))?;
        let dir = repo.join(&build.folder);
        patch(&dir, build)?;
        if !built(&dir, build) {
            // packages of an earlier build (another version or fragment) never reach pacman
            for old in packages(&dir) {
                std::fs::remove_file(&old)
                    .with_context(|| format!("removing {}", old.display()))?;
            }
            progress("image")?;
            let image = build.dir.join("image");
            std::fs::create_dir_all(&image)?;
            std::fs::write(image.join("Dockerfile"), DOCKERFILE)?;
            let uid = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(&build.dir)?);
            run(Command::new("docker")
                .args(["buildx", "build", "--load", "--build-arg"])
                .arg(format!("UID={uid}"))
                .args(["-t", IMAGE])
                .arg(&image))?;
            progress("makepkg")?;
            let log_path = build.dir.join("build.log");
            let log = std::fs::File::create(&log_path)?;
            let status = Command::new("docker")
                .args(["run", "--rm", "-v"])
                .arg(format!("{}:/build", dir.display()))
                .args(["-w", "/build", "-e"])
                .arg(format!("_use_llvm_lto={}", build.lto))
                .args([IMAGE, "makepkg", "-s", "-C", "--noconfirm"])
                .stdout(Stdio::from(log.try_clone()?))
                .stderr(Stdio::from(log))
                .status()
                .context("running docker")?;
            anyhow::ensure!(
                status.success(),
                "makepkg failed ({status}): see {}",
                log_path.display()
            );
            std::fs::write(dir.join(BUILT), record(build))?;
            anyhow::ensure!(
                built(&dir, build),
                "makepkg did not make {} {} ({status}): see {}",
                build.package,
                build.version,
                log_path.display()
            );
        }
        progress("install")?;
        let (name, folder) = (build.package.clone(), dir.display().to_string());
        let lines = self.system(async move |p| p.kernel("install", &name, &folder).await)?;
        progress("done")?;
        Ok(lines)
    }
}
