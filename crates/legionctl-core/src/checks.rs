//! Project checks (design 23, E2; todo-100): a repository declares how it is formatted, linted, built and tested
//! (config key `checks`, repository layer `<repo>/.legionctl/checks.jsonc`); `workspace checkpoint` and `workspace
//! merge` run their gates; each run keeps its log and profile in the dossier home (`.state/runs/<run>/`) and a row in
//! `check_runs`. The language files (`config/languages/*.jsonc`, todo-113) give `code checks detect` its proposal.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::{Dirs, resolve};
use crate::db::now;
use crate::dossier::Dossiers;
use crate::error::{Code, coded};
use crate::git::text as git;
use crate::workspace::git_common_dir;

/// The shipped language files. A new language is a new file in `config/languages/` and a line here.
const LANGUAGES: [&str; 3] = [
    include_str!("../../../config/languages/node.jsonc"),
    include_str!("../../../config/languages/python.jsonc"),
    include_str!("../../../config/languages/rust.jsonc"),
];

/// The lines of a run's log that a refusal and `code checks show` give.
const TAIL: usize = 40;

/// The table of the check runs (dossier.db v2, the step from v1).
macro_rules! table {
    () => {
        "-- one row per run of a check (E2 rule 5); command is what ran, so a changed command needs a new run
     CREATE TABLE check_runs (
        id          TEXT PRIMARY KEY,
        repo        TEXT NOT NULL,
        tree        TEXT NOT NULL,
        name        TEXT NOT NULL,
        kind        TEXT NOT NULL,
        command     TEXT NOT NULL,
        exit        INTEGER NOT NULL,
        wall_ms     INTEGER NOT NULL,
        cpu_ms      INTEGER NOT NULL,
        max_rss_kib INTEGER NOT NULL,
        read_bytes  INTEGER NOT NULL,
        write_bytes INTEGER NOT NULL,
        folder      TEXT NOT NULL,
        job         INTEGER,
        session     TEXT,
        ts          INTEGER NOT NULL
     );
     CREATE INDEX check_runs_tree ON check_runs (repo, tree, name);"
    };
}
pub(crate) use table;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Format,
    Lint,
    Static,
    Types,
    Build,
    Test,
    Deploy,
    Install,
}

impl Kind {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Kind::Format => "format",
            Kind::Lint => "lint",
            Kind::Static => "static",
            Kind::Types => "types",
            Kind::Build => "build",
            Kind::Test => "test",
            Kind::Deploy => "deploy",
            Kind::Install => "install",
        }
    }
}

/// The operation whose gate a check is (E2 rule 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Gate {
    /// `workspace checkpoint` needs a passing run at the tree that it commits.
    Checkpoint,
    /// `workspace merge` needs a passing run at the rebased head.
    Merge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RunnerFormat {
    /// libtest's `test <name> ... ok` lines; with cargo's `--message-format=json`, which names each binary's source.
    Libtest,
}

/// How `code tests run` reads a `test` check's output.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Runner {
    pub format: RunnerFormat,
}

/// One check of a repository (`checks.<name>`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub kind: Kind,
    /// The command (`sh -c`, in the top folder of the workspace, as the user).
    pub run: String,
    /// The command that changes files so that `run` passes; only the main agent runs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<Gate>,
    /// Files the run writes (relative to the workspace) that its run folder keeps.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<String>,
    /// For a `test` check: how `code tests run` reads each test's result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<Runner>,
}

/// The config key `checks`: the settings, and each other key is a check by its name.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ChecksConfig {
    /// The kinds each repository must have; a missing kind is a finding and a nudge.
    pub required: Vec<Kind>,
    /// At session start, tell the agent which required kinds the repository lacks.
    pub nudge: bool,
    /// Run folders kept per check and repository in a dossier; a run that an EVIDENCE front matter names
    /// (`runs: [<run>]`) stays.
    pub keep_runs: usize,
    /// A run that takes longer is killed with its process group and fails.
    pub timeout_minutes: u64,
    /// Check name -> the check.
    #[serde(flatten)]
    pub defs: BTreeMap<String, Check>,
}

/// The tools of a language (todo-113); the first choice of each role.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Tools {
    pub toolchain: String,
    pub lsp: String,
    pub formatter: String,
    pub linter: String,
    pub analyzers: Vec<String>,
    pub profiler: String,
    pub debugger: String,
}

/// A language file: how to find the language in a repository and what to propose for it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Language {
    pub name: String,
    /// A repository is in this language when its top folder has one of these files.
    pub manifests: Vec<String>,
    /// Folders that builds write.
    pub outputs: Vec<String>,
    pub checks: BTreeMap<String, Check>,
    pub tools: Tools,
    /// The capability set that a session in a repository of this language gets (`config/capabilities`).
    pub capability_set: String,
}

/// What a run cost, from the kernel's accounting of the command and everything it waited for.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
pub struct Profile {
    pub wall_ms: u64,
    /// User and system CPU time.
    pub cpu_ms: u64,
    /// The largest resident set of one process of the run.
    pub max_rss_kib: u64,
    /// Bytes read from and written to storage (not the page cache).
    pub read_bytes: u64,
    pub write_bytes: u64,
}

/// One run of a check.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CheckRun {
    /// `<job>-<check>`; its folder is `<dossier home>/.state/runs/<id>/`.
    pub id: String,
    pub check: String,
    pub kind: Kind,
    pub command: String,
    /// The git tree of the working tree when the run started (untracked files too, ignored files not); empty when the
    /// files changed while it ran, so the run is evidence for no tree.
    pub tree: String,
    pub exit: i32,
    pub profile: Profile,
    pub folder: PathBuf,
    pub at: i64,
}

impl CheckRun {
    #[must_use]
    pub fn log(&self) -> PathBuf {
        self.folder.join("log.txt")
    }
}

/// The shipped language files.
///
/// # Errors
/// A shipped file does not parse (a bug of this build).
pub fn languages() -> Result<Vec<Language>> {
    LANGUAGES
        .iter()
        .map(|text| {
            legionctl_machine::profile::parse_jsonc(text)
                .context("a shipped language file does not parse")
        })
        .collect()
}

/// The checks of the repository that contains `dir`, with its repository layer.
///
/// # Errors
/// The config does not resolve, a file of the repository layer has a problem (it would drop the gates), a `deploy` or
/// `install` check has a gate (the user runs them), or `keep_runs` is 0.
pub fn config_at(dir: &Path) -> Result<ChecksConfig> {
    let resolved = resolve(&Dirs::from_env(), Some(dir))?;
    if let Some(layer) = crate::config::repo_root(dir).map(|r| r.join(".legionctl")) {
        let layer = format!("{}/", layer.display());
        let broken: Vec<&str> = resolved
            .problems
            .iter()
            .filter(|p| p.contains(&layer))
            .map(String::as_str)
            .collect();
        ensure!(
            broken.is_empty(),
            "the repository's checks do not resolve, so no gate can pass: {}",
            broken.join("; ")
        );
    }
    let checks = resolved.config.checks;
    for (name, check) in &checks.defs {
        ensure!(
            check.gate.is_none() || ![Kind::Deploy, Kind::Install].contains(&check.kind),
            "check {name} ({}) has a gate: the user runs deploy and install checks, so they are no gates",
            check.kind.name()
        );
    }
    ensure!(
        checks.keep_runs >= 1,
        "checks.keep_runs is 0: it would remove the run that a gate reads"
    );
    Ok(checks)
}

/// The required kinds that the checks of `dir` lack.
///
/// # Errors
/// The config does not resolve.
pub fn missing_kinds(dir: &Path) -> Result<Vec<Kind>> {
    let checks = config_at(dir)?;
    let have: BTreeSet<Kind> = checks.defs.values().map(|c| c.kind).collect();
    Ok(checks
        .required
        .into_iter()
        .filter(|k| !have.contains(k))
        .collect())
}

/// The finding for `missing` kinds: a state, for `doctor` and `dossier check`.
#[must_use]
pub fn missing_state(missing: &[Kind]) -> String {
    let kinds: Vec<&str> = missing.iter().map(|k| k.name()).collect();
    format!("this repository has no checks of kind {}", kinds.join(", "))
}

/// The agent's line for `missing` kinds (the session-start nudge, a refused test run): the state and the command.
#[must_use]
pub fn missing_line(missing: &[Kind]) -> String {
    format!(
        "{}: run `legionctl code checks detect` and write .legionctl/checks.jsonc",
        missing_state(missing)
    )
}

/// The proposed `checks.jsonc` for the repository that contains `dir`, from the language files whose manifest is in
/// its top folder. It writes nothing.
///
/// # Errors
/// No language file matches, or a shipped language file does not parse.
pub fn detect(dir: &Path) -> Result<Vec<String>> {
    let top = crate::config::repo_root(dir).unwrap_or_else(|| dir.to_path_buf());
    let all = languages()?;
    // shortcut: only the top folder is read; a repository with a language in a subfolder gets no proposal for it
    let found: Vec<&Language> = all
        .iter()
        .filter(|l| l.manifests.iter().any(|m| top.join(m).is_file()))
        .collect();
    if found.is_empty() {
        let manifests: Vec<&str> = all
            .iter()
            .flat_map(|l| l.manifests.iter().map(String::as_str))
            .collect();
        return Err(coded(
            Code::NotFound,
            format!(
                "no language file matches {}: it has none of {}",
                top.display(),
                manifests.join(", ")
            ),
        ));
    }
    let names: Vec<&str> = found.iter().map(|l| l.name.as_str()).collect();
    let mut lines = vec![format!(
        "// Proposed by `legionctl code checks detect` from the language files ({}). Review it, then write it to {}.",
        names.join(", "),
        top.join(".legionctl/checks.jsonc").display()
    )];
    let mut checks = serde_json::Map::new();
    for l in &found {
        let t = &l.tools;
        lines.push(format!(
            "// {}: build outputs {}; tools: toolchain {}, lsp {}, formatter {}, linter {}, analyzers {}, profiler {}, \
             debugger {}",
            l.name,
            l.outputs.join(", "),
            t.toolchain,
            t.lsp,
            t.formatter,
            t.linter,
            t.analyzers.join(", "),
            t.profiler,
            t.debugger
        ));
        for (name, check) in &l.checks {
            let key = if found.len() > 1 {
                format!("{}-{name}", l.name)
            } else {
                name.clone()
            };
            checks.insert(key, serde_json::to_value(check)?);
        }
    }
    lines.extend(
        serde_json::to_string_pretty(&serde_json::json!({ "checks": checks }))?
            .lines()
            .map(String::from),
    );
    Ok(lines)
}

/// The git tree of the working tree of `dir`: tracked and untracked files, not the ignored ones. It uses an index
/// file of its own, so the repository's index stays as it is.
///
/// # Errors
/// git fails.
pub fn tree(dir: &Path) -> Result<String> {
    let git_dir = PathBuf::from(git(dir, &["rev-parse", "--absolute-git-dir"])?);
    let index = git_dir.join(format!(
        "legionctl-tree-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    // a copy of the real index keeps git's file stat cache, so unchanged files are not read again
    if git_dir.join("index").is_file() {
        std::fs::copy(git_dir.join("index"), &index)?;
    }
    let out = crate::codetests::git_with(dir, Some(&index), &["add", "--all"])
        .and_then(|_| crate::codetests::git_with(dir, Some(&index), &["write-tree"]));
    let _ = std::fs::remove_file(&index);
    Ok(out?.trim().to_string())
}

/// Run `cmd` in a process group of its own to its end, or kill the group at `limit`: the exit code (128 + the signal
/// for a killed command), what it cost, and whether the limit killed it.
fn profiled(mut cmd: Command, limit: Duration) -> Result<(i32, Profile, bool)> {
    use std::os::unix::process::CommandExt;
    let started = Instant::now();
    let child = cmd.process_group(0).spawn().context("starting sh")?;
    let pid = libc::pid_t::try_from(child.id())?;
    let mut killed = false;
    let (status, usage) = loop {
        let mut status = 0;
        // SAFETY: rusage is plain integers, so all zeros is a valid value; wait4 writes the status and the usage of
        // the child that this process started and has not reaped (std reaps only in wait, which is not called); kill
        // signals the group that this child leads
        let (got, usage) = unsafe {
            let mut usage: libc::rusage = std::mem::zeroed();
            let got = libc::wait4(pid, &raw mut status, libc::WNOHANG, &raw mut usage);
            if got == 0 && !killed && started.elapsed() > limit {
                libc::kill(-pid, libc::SIGKILL);
                killed = true;
            }
            (got, usage)
        };
        if got == pid {
            break (status, usage);
        }
        if got == 0 {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        let e = std::io::Error::last_os_error();
        ensure!(
            e.kind() == std::io::ErrorKind::Interrupted,
            "waiting for the check: {e}"
        );
    };
    drop(child);
    let exit = if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        128 + libc::WTERMSIG(status)
    };
    let ms = |t: libc::timeval| {
        u64::try_from(t.tv_sec).unwrap_or(0) * 1000 + u64::try_from(t.tv_usec).unwrap_or(0) / 1000
    };
    // the kernel counts storage IO in 512-byte blocks
    let bytes = |blocks: libc::c_long| u64::try_from(blocks).unwrap_or(0) * 512;
    Ok((
        exit,
        Profile {
            wall_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            cpu_ms: ms(usage.ru_utime) + ms(usage.ru_stime),
            max_rss_kib: u64::try_from(usage.ru_maxrss).unwrap_or(0),
            read_bytes: bytes(usage.ru_inblock),
            write_bytes: bytes(usage.ru_oublock),
        },
        killed,
    ))
}

/// The last `TAIL` lines of a run's log.
fn tail(log: &Path) -> String {
    let lines: Vec<String> = std::fs::File::open(log)
        .map(|f| BufReader::new(f).lines().map_while(Result::ok).collect())
        .unwrap_or_default();
    lines[lines.len().saturating_sub(TAIL)..].join("\n")
}

/// The hint for a command that was not found: the shell's exit 127, or make's `Error 127` for a recipe line (make
/// itself exits 2). It names the command (the last `<cmd>: not found`, `<cmd>: command not found` or `<cmd>: No such
/// file or directory` line of the log tail) and the fix (todo-131: the operator has the PATH of the systemd user
/// manager, which `setup apply` extends through environment.d).
fn not_found(exit: i32, tail: &str) -> Option<String> {
    if exit != 127 && !tail.lines().any(|l| l.ends_with("] Error 127")) {
        return None;
    }
    let command = tail
        .lines()
        .rev()
        .find_map(|l| {
            let head = l
                .strip_suffix(": not found")
                .or_else(|| l.strip_suffix(": command not found"))
                .or_else(|| l.strip_suffix(": No such file or directory"))?;
            head.rsplit(": ").next()
        })
        .map_or_else(String::new, |c| format!(" ({c})"));
    Some(format!(
        "\ncommand not found in the operator's PATH{command}: run legionctl setup apply and log in again"
    ))
}

/// The run ids that an EVIDENCE front matter of the dossier home names (`runs: [<run>, ...]`), in any subfolder:
/// they stay.
fn evidence_runs(home: &Path) -> BTreeSet<String> {
    fn walk(dir: &Path, runs: &mut BTreeSet<String>) {
        for path in std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
        {
            if path.is_dir() {
                walk(&path, runs);
            } else if let Ok(text) = std::fs::read_to_string(&path) {
                runs.extend(
                    crate::graph::front_matter(&text)
                        .remove("runs")
                        .unwrap_or_default(),
                );
            }
        }
    }
    let mut runs = BTreeSet::new();
    walk(&home.join("EVIDENCE"), &mut runs);
    runs
}

/// Remove the run folders of check `name` past `keep`, unless an EVIDENCE names them; the rows stay.
fn prune_runs(
    db: &crate::db::Db,
    home: &Path,
    (repo, name): (&str, &str),
    keep: usize,
) -> Result<()> {
    let named = evidence_runs(home);
    let mut stmt = db.conn().prepare(
        "SELECT id FROM check_runs WHERE repo = ?1 AND name = ?2 ORDER BY ts DESC, rowid DESC LIMIT -1 OFFSET ?3",
    )?;
    let old = stmt
        .query_map(
            params![repo, name, i64::try_from(keep).unwrap_or(i64::MAX)],
            |r| r.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in old.into_iter().filter(|id| !named.contains(id)) {
        match std::fs::remove_dir_all(home.join(".state/runs").join(&id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    Ok(())
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<CheckRun> {
    let kind: String = r.get("kind")?;
    let n = |c: &str| r.get::<_, i64>(c).map(|v| u64::try_from(v).unwrap_or(0));
    Ok(CheckRun {
        id: r.get("id")?,
        check: r.get("name")?,
        kind: serde_json::from_value(serde_json::Value::String(kind)).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        command: r.get("command")?,
        tree: r.get("tree")?,
        exit: r.get("exit")?,
        profile: Profile {
            wall_ms: n("wall_ms")?,
            cpu_ms: n("cpu_ms")?,
            max_rss_kib: n("max_rss_kib")?,
            read_bytes: n("read_bytes")?,
            write_bytes: n("write_bytes")?,
        },
        folder: PathBuf::from(r.get::<_, String>("folder")?),
        at: r.get("ts")?,
    })
}

fn repo_key(dir: &Path) -> Result<String> {
    Ok(git_common_dir(dir)?.to_string_lossy().into_owned())
}

fn top(dir: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(git(dir, &["rev-parse", "--show-toplevel"])?))
}

impl Dossiers {
    /// Run check `name` in the workspace of `dir` (with `args` after its command; in `jail` for a session, O-1) and keep
    /// its log, profile and `profiles` files in the writer dossier's `.state/runs/<job>-<name>/` and its row in
    /// `check_runs`. A failed command is a run with its exit code, not an error.
    ///
    /// # Errors
    /// No dossier is attached to the repository, the command cannot start, or a write fails.
    pub fn run_check(
        &self,
        dir: &Path,
        (name, check): (&str, &Check),
        args: &[&str],
        job: i64,
        (session, jail): (Option<&str>, Option<&crate::session::Jail>),
    ) -> Result<CheckRun> {
        let (top, repo, tree) = (top(dir)?, repo_key(dir)?, tree(dir)?);
        let dossier = self.writer_name(dir)?;
        let home = self.root.join(&dossier);
        let id = format!("{job}-{name}");
        let folder = home.join(".state/runs").join(&id);
        std::fs::create_dir_all(&folder)?;
        let log = std::fs::File::create(folder.join("log.txt"))?;
        let mut cmd = match jail {
            Some(jail) => jail.command(&top, &["sh"]),
            None => Command::new("sh"),
        };
        if args.is_empty() {
            cmd.arg("-c").arg(&check.run);
        } else {
            // the arguments are sh's positional parameters, never shell text
            cmd.arg("-c")
                .arg(format!("{} \"$@\"", check.run))
                .arg("sh")
                .args(args);
        }
        cmd.current_dir(&top)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        let checks = config_at(dir)?;
        let limit = checks.timeout_minutes;
        let (exit, profile, killed) = profiled(cmd, Duration::from_secs(limit.saturating_mul(60)))?;
        if killed {
            use std::io::Write;
            writeln!(
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(folder.join("log.txt"))?,
                "legionctl: killed after checks.timeout_minutes ({limit} minutes)"
            )?;
        }
        // a fixing tool, a generator or a peer changed the files: the run is evidence for no tree
        let tree = if crate::checks::tree(dir)? == tree {
            tree
        } else {
            String::new()
        };
        std::fs::write(
            folder.join("profile.json"),
            serde_json::to_string_pretty(&profile)?,
        )?;
        for file in &check.profiles {
            let source = top.join(file);
            if let (true, Some(base)) = (source.is_file(), source.file_name()) {
                std::fs::copy(&source, folder.join(base))?;
            }
        }
        let run = CheckRun {
            id,
            check: name.to_string(),
            kind: check.kind,
            command: [check.run.as_str()]
                .into_iter()
                .chain(args.iter().copied())
                .collect::<Vec<_>>()
                .join(" "),
            tree,
            exit,
            profile,
            folder,
            at: now(),
        };
        let db = self.home(&dossier)?;
        let i = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
        db.conn().execute(
            "INSERT INTO check_runs (id, repo, tree, name, kind, command, exit, wall_ms, cpu_ms, max_rss_kib,
                                     read_bytes, write_bytes, folder, job, session, ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                run.id,
                repo,
                run.tree,
                name,
                check.kind.name(),
                run.command,
                exit,
                i(profile.wall_ms),
                i(profile.cpu_ms),
                i(profile.max_rss_kib),
                i(profile.read_bytes),
                i(profile.write_bytes),
                run.folder.to_string_lossy(),
                job,
                session,
                run.at
            ],
        )?;
        prune_runs(&db, &home, (&repo, name), checks.keep_runs)?;
        Ok(run)
    }

    /// The passing run of check `name` with `command` at `tree`, if any (in any dossier attached to the repository).
    fn passing(
        &self,
        dir: &Path,
        tree: &str,
        (name, command): (&str, &str),
    ) -> Result<Option<String>> {
        Ok(self
            .facts(dir)?
            .query_row(
                "SELECT id FROM check_runs WHERE repo = ?1 AND tree = ?2 AND name = ?3 AND command = ?4 AND exit = 0
                 ORDER BY ts DESC LIMIT 1",
                params![repo_key(dir)?, tree, name, command],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// The gate of `workspace checkpoint` or `workspace merge` (E2 rule 4): each check of `gate` needs a passing run
    /// at the tree of `dir`; one without it runs now, as job `code.checks.run`. Returns one line per check.
    ///
    /// # Errors
    /// A check fails (the error names the check, its exit code and log, and ends with the log's last lines), or a
    /// run cannot start.
    pub fn gate(
        &self,
        dir: &Path,
        gate: Gate,
        (session, jail): (Option<&str>, Option<&crate::session::Jail>),
    ) -> Result<Vec<String>> {
        let checks = config_at(dir)?;
        let wanted: Vec<(&String, &Check)> = checks
            .defs
            .iter()
            .filter(|(_, c)| c.gate == Some(gate))
            .collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let tree = tree(dir)?;
        let mut job = None;
        let mut out = || -> Result<Vec<String>> {
            let mut lines = Vec::new();
            for (name, check) in &wanted {
                if let Some(id) = self.passing(dir, &tree, (name, &check.run))? {
                    lines.push(format!("check {name}: passed at this tree (run {id})"));
                    continue;
                }
                let id = match job {
                    Some(id) => id,
                    None => *job.insert(self.operator().job_begin(
                        "code.checks.run",
                        &serde_json::json!({"dir": dir, "gate": gate}),
                    )?),
                };
                let run = self.run_check(dir, (name, check), &[], id, (session, jail))?;
                if run.tree.is_empty() {
                    return Err(coded(
                        Code::Conflict,
                        format!(
                            "check {name} ({}) changed the files of the workspace while it ran; log {}: \
                             check the change, then run the operation again",
                            check.kind.name(),
                            run.log().display()
                        ),
                    ));
                }
                if run.exit != 0 {
                    let tail = tail(&run.log());
                    let hint = not_found(run.exit, &tail).unwrap_or_default();
                    return Err(coded(
                        Code::Conflict,
                        format!(
                            "check {name} ({}) failed with exit {}; log {}{hint}\n{tail}",
                            check.kind.name(),
                            run.exit,
                            run.log().display(),
                        ),
                    ));
                }
                lines.push(format!(
                    "check {name}: passed (run {}, {} ms)",
                    run.id, run.profile.wall_ms
                ));
            }
            Ok(lines)
        };
        let lines = out();
        if let Some(id) = job {
            let outcome = match &lines {
                Ok(l) => Ok(serde_json::json!({ "lines": l })),
                Err(e) => Err(format!("{e:#}")),
            };
            self.operator().job_end(id, &outcome)?;
        }
        lines
    }

    /// Run the checks `names` (none: every check but `deploy` and `install`) in the workspace of `dir` as one job.
    ///
    /// # Errors
    /// A name is not a check, a named check is a `deploy` or `install` check, or a run cannot start.
    pub fn run_checks(
        &self,
        dir: &Path,
        names: &[String],
        (session, jail): (Option<&str>, Option<&crate::session::Jail>),
    ) -> Result<Vec<CheckRun>> {
        let checks = config_at(dir)?;
        for name in names {
            let check = checks.defs.get(name).ok_or_else(|| {
                coded(
                    Code::NotFound,
                    format!(
                        "no check {name}; the checks: {}",
                        checks.defs.keys().cloned().collect::<Vec<_>>().join(", ")
                    ),
                )
            })?;
            if [Kind::Deploy, Kind::Install].contains(&check.kind) {
                return Err(coded(
                    Code::Denied,
                    format!(
                        "check {name} is a {} check: it needs the user's approval, so the user runs `{}`",
                        check.kind.name(),
                        check.run
                    ),
                ));
            }
        }
        let job = self.operator().job_begin(
            "code.checks.run",
            &serde_json::json!({"dir": dir, "checks": names}),
        )?;
        let out = checks
            .defs
            .iter()
            .filter(|(n, c)| {
                if names.is_empty() {
                    ![Kind::Deploy, Kind::Install].contains(&c.kind)
                } else {
                    names.contains(n)
                }
            })
            .map(|(n, c)| self.run_check(dir, (n, c), &[], job, (session, jail)))
            .collect::<Result<Vec<_>>>();
        let outcome = match &out {
            Ok(runs) => Ok(serde_json::to_value(runs)?),
            Err(e) => Err(format!("{e:#}")),
        };
        self.operator().job_end(job, &outcome)?;
        out
    }

    /// One run by its id (`code checks show`), from any dossier attached to the repository, and its log's last lines.
    ///
    /// # Errors
    /// No run has this id.
    pub fn check_run(&self, dir: &Path, id: &str) -> Result<(CheckRun, String)> {
        let run = self
            .facts(dir)?
            .query_row("SELECT * FROM check_runs WHERE id = ?1", [id], row)
            .optional()?
            .ok_or_else(|| coded(Code::NotFound, format!("no check run {id}")))?;
        let log = tail(&run.log());
        Ok((run, log))
    }

    /// The checks of `dir` with the file that set each and its last run (`code checks list`).
    ///
    /// # Errors
    /// The config does not resolve, or a database read fails.
    pub fn checks_list(&self, dir: &Path) -> Result<Vec<String>> {
        let resolved = resolve(&Dirs::from_env(), Some(dir))?;
        let facts = self.facts(dir)?;
        let repo = repo_key(dir)?;
        let mut lines = Vec::new();
        for (name, check) in &resolved.config.checks.defs {
            let file = resolved
                .trace
                .get(&format!("checks.{name}.run"))
                .and_then(|s| s.last())
                .map_or_else(String::new, |s| s.source.file.clone());
            let last = facts
                .query_row(
                    "SELECT * FROM check_runs WHERE repo = ?1 AND name = ?2 ORDER BY ts DESC, rowid DESC LIMIT 1",
                    params![repo, name],
                    row,
                )
                .optional()?;
            lines.push(format!(
                "{name} ({}{}): {} [{file}]; last run: {}",
                check.kind.name(),
                check.gate.map_or(String::new(), |g| format!(
                    ", gate {}",
                    serde_json::to_value(g)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from))
                        .unwrap_or_default()
                )),
                check.run,
                last.map_or_else(
                    || "none".into(),
                    |r| format!(
                        "{} exit {} at tree {}",
                        r.id,
                        r.exit,
                        &r.tree[..r.tree.len().min(12)]
                    )
                )
            ));
        }
        Ok(lines)
    }

    /// The findings of the checks of `dir` (doctor, `dossier check`): the required kinds it lacks, each check whose
    /// last run did not find its command (exit 127), or why its checks do not resolve; a worktree that is gone is
    /// that finding (its checks cannot be read).
    #[must_use]
    pub fn check_findings(&self, dir: &Path) -> Vec<String> {
        if !dir.is_dir() {
            return vec![format!(
                "the worktree {} is missing; open the workspace again",
                dir.display()
            )];
        }
        self.findings_of(dir).unwrap_or_else(|e| {
            vec![format!(
                "the checks of {} do not resolve: {e:#}",
                dir.display()
            )]
        })
    }

    fn findings_of(&self, dir: &Path) -> Result<Vec<String>> {
        let mut findings = Vec::new();
        let missing = missing_kinds(dir)?;
        if !missing.is_empty() {
            findings.push(format!("{}: {}", dir.display(), missing_state(&missing)));
        }
        let facts = self.facts(dir)?;
        let repo = repo_key(dir)?;
        for (name, check) in config_at(dir)?.defs {
            let exit: Option<i32> = facts
                .query_row(
                    "SELECT exit FROM check_runs WHERE repo = ?1 AND name = ?2 AND command = ?3
                     ORDER BY ts DESC, rowid DESC LIMIT 1",
                    params![repo, name, check.run],
                    |r| r.get(0),
                )
                .optional()?;
            if exit == Some(127) {
                findings.push(format!(
                    "{}: check {name} broken: its command was not found in the operator's PATH (exit 127): {}",
                    dir.display(),
                    check.run
                ));
            }
        }
        Ok(findings)
    }
}
