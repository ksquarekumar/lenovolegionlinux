//! Tests as evidence (design 23, A7): test runs per test, mutant records (from `cargo-mutants` on the files of the
//! change set, and from the agents' own mutation checks), the review signals of the new and changed tests of a change
//! set, and the gate that `workspace checkpoint` runs before it commits. The records are in `workspace.db`, keyed by
//! the repository (its git common folder, so that every worktree of one repository shares them).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::params;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::now;
use crate::dossier::Dossiers;
use crate::workspace::{git, git_common_dir};

/// The tables of the test records (workspace.db v12).
macro_rules! tables {
    () => {
        "-- one row per test per run of `code tests run` (A7): file is the source of the test binary
     CREATE TABLE test_runs (
        repo   TEXT NOT NULL,
        ref    TEXT NOT NULL,
        file   TEXT NOT NULL,
        test   TEXT NOT NULL,
        result TEXT NOT NULL,
        job    INTEGER,
        ts     INTEGER NOT NULL
     );
     CREATE INDEX test_runs_test ON test_runs (repo, test);
     -- mutants: killed is the JSON list of the tests that failed with the mutation; source tool or agent
     CREATE TABLE mutants (
        id          INTEGER PRIMARY KEY,
        repo        TEXT NOT NULL,
        ref         TEXT NOT NULL,
        file        TEXT NOT NULL,
        line        INTEGER NOT NULL,
        description TEXT NOT NULL,
        killed      TEXT NOT NULL,
        source      TEXT NOT NULL,
        ts          INTEGER NOT NULL
     );
     -- tests that the gate lets through without evidence (`workspace checkpoint --accept-test --reason`)
     CREATE TABLE test_accepts (
        repo    TEXT NOT NULL,
        test    TEXT NOT NULL,
        reason  TEXT NOT NULL,
        session TEXT NOT NULL,
        ts      INTEGER NOT NULL
     );"
    };
}
pub(crate) use tables;

/// The command of `code tests run`; the filter goes after `--`.
const TEST_COMMAND: &str = "cargo test --release --workspace --no-fail-fast -j8";

/// One mutation and the tests that failed with it (none: the mutant survived).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Mutant {
    pub file: String,
    pub line: u32,
    pub description: String,
    pub killed_by: Vec<String>,
    /// `tool` (cargo-mutants) or `agent` (a hand-made mutation).
    pub source: String,
    pub git_ref: String,
    pub at: i64,
}

/// One result of a test in a run.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TestRun {
    pub file: String,
    pub result: String,
    pub git_ref: String,
    pub job: Option<i64>,
    pub at: i64,
}

/// A test that `workspace checkpoint --accept-test` let through, and why.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Accept {
    pub reason: String,
    pub session: String,
    pub at: i64,
}

/// A test of the repository: its last run and its kills.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TestRow {
    pub name: String,
    pub file: String,
    pub result: String,
    pub git_ref: String,
    pub at: i64,
    pub kills: u32,
}

/// Everything recorded for one test.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TestDetail {
    pub name: String,
    pub runs: Vec<TestRun>,
    pub mutants: Vec<Mutant>,
    pub accepts: Vec<Accept>,
}

/// The review of one new or changed test.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReviewedTest {
    pub name: String,
    pub file: String,
    pub assertions: u32,
    pub kills: u32,
    /// In a `tests/` folder: it reaches the code through the crate's boundary, not its internals.
    pub boundary: bool,
    /// A test that kills each mutant this one kills.
    pub duplicate_of: Option<String>,
    /// The hard signals: the gate refuses the test for these.
    pub refused: Vec<String>,
    /// The soft signals.
    pub notes: Vec<String>,
    /// The reason of `--accept-test`, when the test was let through.
    pub accepted: Option<String>,
}

/// The new and changed tests of a change set.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TestReview {
    pub tests: Vec<ReviewedTest>,
}

impl TestReview {
    /// The tests the gate refuses, one line each with the reasons.
    #[must_use]
    pub fn refusals(&self) -> Vec<String> {
        self.tests
            .iter()
            .filter(|t| t.accepted.is_none() && !t.refused.is_empty())
            .map(|t| format!("{} ({}): {}", t.name, t.file, t.refused.join("; ")))
            .collect()
    }
}

/// The repository key of `dir`: its git common folder.
fn repo_key(dir: &Path) -> Result<String> {
    Ok(git_common_dir(dir)?.to_string_lossy().into_owned())
}

/// A test function: its name and the lines from its attribute to its closing brace (1-based).
struct TestFn {
    name: String,
    start: usize,
    end: usize,
    body: String,
}

/// The `#[test]` functions of a Rust file.
// shortcut: braces are counted without reading strings or char literals; an unbalanced `'{'` in a test body ends the
// span early. The code graph has spans for committed trees only (ctx); upgrade when it indexes the index.
fn tests_in(text: &str) -> Vec<TestFn> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let attr = lines[i].trim();
        if attr != "#[test]" && !attr.starts_with("#[tokio::test") {
            i += 1;
            continue;
        }
        let Some(head) = (i..lines.len()).find(|&j| lines[j].contains("fn ")) else {
            break;
        };
        let name: String = lines[head]
            .split("fn ")
            .nth(1)
            .unwrap_or_default()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let (mut depth, mut opened, mut end) = (0i32, false, head);
        for (j, line) in lines.iter().enumerate().skip(head) {
            for c in line.chars() {
                match c {
                    '{' => (depth, opened) = (depth + 1, true),
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            end = j;
            if opened && depth == 0 {
                break;
            }
        }
        found.push(TestFn {
            name,
            start: i + 1,
            end: end + 1,
            body: lines[head..=end].join("\n"),
        });
        i = end + 1;
    }
    found
}

/// The assertions in a test body: `assert*!`, `panic!`, `unwrap_err`, `expect_err`.
fn assertions(body: &str) -> u32 {
    let macros = body
        .match_indices("assert")
        .filter(|(at, _)| {
            body[at + "assert".len()..]
                .chars()
                .find(|c| !(c.is_alphanumeric() || *c == '_'))
                == Some('!')
        })
        .count();
    let others: usize = ["panic!", "unwrap_err", "expect_err"]
        .iter()
        .map(|w| body.matches(w).count())
        .sum();
    u32::try_from(macros + others).unwrap_or(u32::MAX)
}

/// A recorded test name (libtest's, with its module path) is the test function `name`.
fn names(recorded: &str, name: &str) -> bool {
    recorded == name || recorded.ends_with(&format!("::{name}"))
}

/// The changed Rust files against `base` (in the index with `staged`, else in the working tree with the untracked
/// files) and the new-side lines each change touches.
fn change_set(
    dir: &Path,
    base: &str,
    staged: bool,
) -> Result<BTreeMap<String, Vec<(usize, usize)>>> {
    let mut args = vec!["diff", "-U0", "--no-color", "--no-ext-diff"];
    if staged {
        args.push("--cached");
    }
    args.extend([base, "--", "*.rs"]);
    let mut files: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    let mut file = None;
    for line in git(dir, &args)?.lines() {
        if let Some(path) = line.strip_prefix("+++ ") {
            file = path.strip_prefix("b/").map(String::from);
        } else if let (Some(hunk), Some(f)) = (line.strip_prefix("@@ "), &file) {
            // `@@ -a,b +c,d @@`: new lines c..c+d; d = 0 is a removal after line c
            let new = hunk
                .split(' ')
                .nth(1)
                .unwrap_or_default()
                .trim_start_matches('+');
            let (start, count) = new.split_once(',').unwrap_or((new, "1"));
            let start: usize = start.parse().unwrap_or(0);
            let count: usize = count.parse().unwrap_or(1);
            files
                .entry(f.clone())
                .or_default()
                .push((start.max(1), start + count.max(1) - 1));
        }
    }
    if !staged {
        for path in git(
            dir,
            &["ls-files", "--others", "--exclude-standard", "--", "*.rs"],
        )?
        .lines()
        {
            files.insert(path.to_string(), vec![(1, usize::MAX)]);
        }
    }
    Ok(files)
}

/// The text of `path` in the change set: the index with `staged`, else the working tree.
fn content(dir: &Path, path: &str, staged: bool) -> Result<String> {
    if !staged {
        return std::fs::read_to_string(dir.join(path)).with_context(|| format!("reading {path}"));
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["show", &format!(":{path}")])
        .output()
        .context("running git")?;
    ensure!(
        out.status.success(),
        "git show :{path}: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The mutants of a cargo-mutants run in `results` (its `mutants.out`), each with the tests that failed in its log.
fn tool_mutants(results: &Path) -> Result<Vec<Mutant>> {
    let path = results.join("outcomes.json");
    let o: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)
        .with_context(|| format!("reading {}", path.display()))?;
    let mut found = Vec::new();
    for outcome in o["outcomes"].as_array().into_iter().flatten() {
        let mutant = &outcome["scenario"]["Mutant"];
        let summary = outcome["summary"].as_str().unwrap_or_default();
        // unviable mutants do not build and timeouts name no test: neither is evidence for a test
        if mutant.is_null() || !["CaughtMutant", "MissedMutant"].contains(&summary) {
            continue;
        }
        let log =
            std::fs::read_to_string(results.join(outcome["log_path"].as_str().unwrap_or_default()))
                .unwrap_or_default();
        found.push(Mutant {
            file: mutant["file"].as_str().unwrap_or_default().into(),
            line: mutant["span"]["start"]["line"]
                .as_u64()
                .map_or(0, |l| u32::try_from(l).unwrap_or(0)),
            description: mutant["name"].as_str().unwrap_or_default().into(),
            killed_by: log
                .lines()
                .filter_map(|l| l.strip_prefix("test ")?.strip_suffix(" ... FAILED"))
                .map(String::from)
                .collect(),
            source: "tool".into(),
            git_ref: String::new(),
            at: 0,
        });
    }
    Ok(found)
}

impl Dossiers {
    fn mutants(&self, repo: &str) -> Result<Vec<Mutant>> {
        let mut stmt = self.db().prepare(
            "SELECT file, line, description, killed, source, ref, ts FROM mutants WHERE repo = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([repo], |r| {
            Ok(Mutant {
                file: r.get(0)?,
                line: r.get(1)?,
                description: r.get(2)?,
                killed_by: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
                source: r.get(4)?,
                git_ref: r.get(5)?,
                at: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn accepts(&self, repo: &str, test: &str) -> Result<Vec<Accept>> {
        let mut stmt = self
            .db()
            .prepare("SELECT reason, session, ts FROM test_accepts WHERE repo = ?1 AND test = ?2 ORDER BY ts")?;
        let rows = stmt.query_map([repo, test], |r| {
            Ok(Accept {
                reason: r.get(0)?,
                session: r.get(1)?,
                at: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn insert_mutant(&self, repo: &str, git_ref: &str, m: &Mutant) -> Result<()> {
        self.db().execute(
            "INSERT INTO mutants (repo, ref, file, line, description, killed, source, ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                repo,
                git_ref,
                m.file,
                m.line,
                m.description,
                serde_json::to_string(&m.killed_by)?,
                m.source,
                now()
            ],
        )?;
        Ok(())
    }

    /// Record a mutation an agent made by hand and the tests that failed with it.
    ///
    /// # Errors
    /// `dir` is not in a git repository, or the database write fails.
    pub fn record_mutant(
        &self,
        dir: &Path,
        file: &str,
        line: u32,
        description: &str,
        killed_by: Vec<String>,
    ) -> Result<String> {
        ensure!(
            !description.trim().is_empty(),
            "a mutant needs a description of the mutation"
        );
        let m = Mutant {
            file: file.into(),
            line,
            description: description.into(),
            killed_by,
            source: "agent".into(),
            git_ref: String::new(),
            at: 0,
        };
        self.insert_mutant(&repo_key(dir)?, &git(dir, &["rev-parse", "HEAD"])?, &m)?;
        Ok(if m.killed_by.is_empty() {
            format!("recorded a mutant that no test killed: {file}:{line} {description}")
        } else {
            format!(
                "recorded {file}:{line} {description}, killed by {}",
                m.killed_by.join(", ")
            )
        })
    }

    /// Let `test` through the gate without evidence, for `reason`.
    ///
    /// # Errors
    /// The reason is empty, `dir` is not in a git repository, or the database write fails.
    pub fn accept_test(&self, dir: &Path, test: &str, reason: &str, session: &str) -> Result<()> {
        ensure!(
            !reason.trim().is_empty(),
            "--accept-test needs --reason <why the test has value>"
        );
        self.db().execute(
            "INSERT INTO test_accepts (repo, test, reason, session, ts) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![repo_key(dir)?, test, reason, session, now()],
        )?;
        Ok(())
    }

    /// The review of the new and changed tests of the change set against HEAD (`staged`: the index, else the working
    /// tree): assertions, kills, duplicates, place, and the hard signals the gate refuses.
    ///
    /// # Errors
    /// git fails in `dir`, or the database read fails.
    pub fn review_tests(&self, dir: &Path, staged: bool) -> Result<TestReview> {
        let repo = repo_key(dir)?;
        let mutants = self.mutants(&repo)?;
        // every killer's set of mutants, for the kills and the duplicates
        let mut killed: BTreeMap<&str, BTreeSet<usize>> = BTreeMap::new();
        for (i, m) in mutants.iter().enumerate() {
            for t in &m.killed_by {
                killed.entry(t).or_default().insert(i);
            }
        }
        let mut tests = Vec::new();
        for (file, hunks) in change_set(dir, "HEAD", staged)? {
            let text = content(dir, &file, staged)?;
            for t in tests_in(&text) {
                if !hunks.iter().any(|&(a, b)| a <= t.end && t.start <= b) {
                    continue;
                }
                let own: BTreeSet<usize> = killed
                    .iter()
                    .filter(|(k, _)| names(k, &t.name))
                    .flat_map(|(_, s)| s.iter().copied())
                    .collect();
                let duplicate_of = (!own.is_empty())
                    .then(|| {
                        killed
                            .iter()
                            .find(|(k, s)| !names(k, &t.name) && own.is_subset(s))
                            .map(|(k, _)| (*k).to_string())
                    })
                    .flatten();
                let boundary = Path::new(&file)
                    .components()
                    .any(|c| c.as_os_str() == "tests");
                let count = assertions(&t.body);
                let mut refused = Vec::new();
                if count == 0 {
                    refused.push(
                        "no assertion (assert!, assert_eq!, assert_ne!, panic!, unwrap_err)".into(),
                    );
                }
                if own.is_empty() {
                    refused.push(
                        "kills no mutant: record a mutation it catches (code tests mutant record) or run code tests mutant run"
                            .into(),
                    );
                }
                let mut notes = Vec::new();
                if !boundary {
                    notes.push(
                        "a unit test outside tests/: the rule is e2e tests from the boundary"
                            .into(),
                    );
                }
                if let Some(d) = &duplicate_of {
                    notes.push(format!("duplicate: {d} kills each mutant this test kills"));
                }
                let accepted = self.accepts(&repo, &t.name)?.pop().map(|a| a.reason);
                tests.push(ReviewedTest {
                    kills: u32::try_from(own.len()).unwrap_or(u32::MAX),
                    name: t.name,
                    file: file.clone(),
                    assertions: count,
                    boundary,
                    duplicate_of,
                    refused,
                    notes,
                    accepted,
                });
            }
        }
        Ok(TestReview { tests })
    }

    /// The gate of `workspace checkpoint`: refuse the staged change set when a new or changed test has a hard signal
    /// and no accept.
    ///
    /// # Errors
    /// A test is refused (the error names each test and its reasons), or the review fails.
    pub fn test_gate(&self, dir: &Path) -> Result<()> {
        let refusals = self.review_tests(dir, true)?.refusals();
        if refusals.is_empty() {
            return Ok(());
        }
        Err(crate::error::coded(
            crate::error::Code::Conflict,
            format!(
                "checkpoint refused, nothing committed; the test gate refuses:\n  {}\nGive evidence (skill \
                 verify-and-ship), or let one test through: --accept-test <test> --reason <why>",
                refusals.join("\n  ")
            ),
        ))
    }

    /// Run the test suite in `dir` (`cargo test`, with `filter` after `--`) and record each test's result;
    /// `progress` gets the counts of results so far.
    ///
    /// # Errors
    /// The command cannot start, or a database write fails.
    pub fn run_tests(
        &self,
        dir: &Path,
        filter: Option<&str>,
        job: i64,
        mut progress: impl FnMut(&BTreeMap<String, u32>),
    ) -> Result<Vec<String>> {
        let (repo, head) = (repo_key(dir)?, git(dir, &["rev-parse", "HEAD"])?);
        // cargo writes `Running <file>` to stderr and the results to stdout: one stream keeps their order
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(format!("{TEST_COMMAND} -- \"$@\" 2>&1"))
            .arg("sh")
            .args(filter)
            .current_dir(dir)
            .stdout(Stdio::piped())
            .spawn()
            .context("starting cargo test")?;
        let mut counts: BTreeMap<String, u32> = BTreeMap::new();
        let mut file = String::new();
        for line in BufReader::new(child.stdout.take().context("no output")?).lines() {
            let line = line?;
            if let Some(rest) = line.trim().strip_prefix("Running ") {
                // `Running tests/e2e.rs (target/...)`, `Running unittests src/lib.rs (target/...)`
                let rest = rest.strip_prefix("unittests ").unwrap_or(rest);
                file = rest.split(" (").next().unwrap_or(rest).to_string();
            } else if let Some((name, result)) = line
                .strip_prefix("test ")
                .and_then(|l| l.rsplit_once(" ... "))
                .filter(|(_, r)| ["ok", "FAILED", "ignored"].contains(r))
            {
                let result = result.to_lowercase();
                self.db().execute(
                    "INSERT INTO test_runs (repo, ref, file, test, result, job, ts) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![repo, head, file, name, result, job, now()],
                )?;
                *counts.entry(result).or_default() += 1;
                progress(&counts);
            }
        }
        let status = child.wait()?;
        let summary: Vec<String> = counts.iter().map(|(r, n)| format!("{n} {r}")).collect();
        Ok(vec![format!(
            "{TEST_COMMAND}: {} ({status})",
            if summary.is_empty() {
                "no test ran".into()
            } else {
                summary.join(", ")
            }
        )])
    }

    /// Run `cargo-mutants` on the Rust source files that the branch of `dir` changed since `base` and record each
    /// mutant with the tests that failed with it; `progress` gets (done, total).
    ///
    /// # Errors
    /// The change set has no source file, cargo-mutants cannot start or fails, or its output does not parse.
    pub fn run_mutants(
        &self,
        dir: &Path,
        base: &str,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<Vec<String>> {
        let (repo, head) = (repo_key(dir)?, git(dir, &["rev-parse", "HEAD"])?);
        let from = git(dir, &["merge-base", "HEAD", base])?;
        let files: Vec<String> = change_set(dir, &from, false)?
            .into_keys()
            .filter(|f| !Path::new(f).components().any(|c| c.as_os_str() == "tests"))
            .filter(|f| dir.join(f).is_file())
            .collect();
        ensure!(
            !files.is_empty(),
            "the change set since {base} has no Rust source file outside tests/ to mutate"
        );
        let out = dir.join("target");
        let results = out.join("mutants.out");
        let _ = std::fs::remove_dir_all(&results);
        std::fs::create_dir_all(&out)?;
        let log = std::fs::File::create(out.join("cargo-mutants.log"))?;
        let mut cmd = Command::new("cargo-mutants");
        cmd.args([
            "mutants",
            "--no-shuffle",
            "--test-workspace=true",
            "--cargo-test-arg=--no-fail-fast",
            "-o",
        ])
        .arg(&out);
        for f in &files {
            cmd.args(["--file", f]);
        }
        let mut child = cmd
            .current_dir(dir)
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()
            .context("starting cargo-mutants (cargo install cargo-mutants --locked)")?;
        let outcomes = results.join("outcomes.json");
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            // cargo-mutants writes outcomes.json again after each mutant
            let now: Option<serde_json::Value> = std::fs::read(&outcomes)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
            if let Some(o) = now {
                // the first outcome is the baseline
                let done = o["outcomes"]
                    .as_array()
                    .map_or(0, |a| a.len().saturating_sub(1));
                progress(
                    u64::try_from(done).unwrap_or(0),
                    o["total_mutants"].as_u64().unwrap_or(0),
                );
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        };
        // 0: all caught, 2: some missed, 3: some timed out; the rest is a failed run
        if !matches!(status.code(), Some(0 | 2 | 3)) {
            let text = std::fs::read_to_string(out.join("cargo-mutants.log")).unwrap_or_default();
            let tail: Vec<&str> = text.lines().rev().take(20).collect();
            bail!(
                "cargo-mutants failed ({status}):\n{}",
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            );
        }
        let mutants = tool_mutants(&results)?;
        let mut missed = Vec::new();
        for m in &mutants {
            if m.killed_by.is_empty() {
                missed.push(format!("  missed: {}", m.description));
            }
            self.insert_mutant(&repo, &head, m)?;
        }
        let caught = mutants.len() - missed.len();
        let mut lines = vec![format!(
            "cargo-mutants on {}: {caught} caught, {} missed",
            files.join(", "),
            missed.len()
        )];
        lines.extend(missed);
        Ok(lines)
    }

    /// Each test of the repository of `dir` with its last run and its kills.
    ///
    /// # Errors
    /// `dir` is not in a git repository, or the database read fails.
    pub fn tests_list(&self, dir: &Path) -> Result<Vec<TestRow>> {
        let repo = repo_key(dir)?;
        let mutants = self.mutants(&repo)?;
        let mut stmt = self.db().prepare(
            "SELECT test, file, result, ref, MAX(ts) FROM test_runs WHERE repo = ?1 GROUP BY test ORDER BY file, test",
        )?;
        let rows = stmt.query_map([&repo], |r| {
            Ok(TestRow {
                name: r.get(0)?,
                file: r.get(1)?,
                result: r.get(2)?,
                git_ref: r.get(3)?,
                at: r.get(4)?,
                kills: 0,
            })
        })?;
        let mut rows: Vec<TestRow> = rows.collect::<rusqlite::Result<_>>()?;
        for row in &mut rows {
            let kills = mutants
                .iter()
                .filter(|m| m.killed_by.contains(&row.name))
                .count();
            row.kills = u32::try_from(kills).unwrap_or(u32::MAX);
        }
        Ok(rows)
    }

    /// The runs, the mutants it killed and the accepts of the test `name` (the function name or libtest's).
    ///
    /// # Errors
    /// Nothing is recorded for the test, or the database read fails.
    pub fn test_show(&self, dir: &Path, name: &str) -> Result<TestDetail> {
        let repo = repo_key(dir)?;
        let mut stmt = self.db().prepare(
            "SELECT test, file, result, ref, job, ts FROM test_runs WHERE repo = ?1 ORDER BY ts DESC, rowid DESC",
        )?;
        let runs = stmt.query_map([&repo], |r| {
            Ok((
                r.get::<_, String>(0)?,
                TestRun {
                    file: r.get(1)?,
                    result: r.get(2)?,
                    git_ref: r.get(3)?,
                    job: r.get(4)?,
                    at: r.get(5)?,
                },
            ))
        })?;
        let mut detail = TestDetail {
            name: name.into(),
            runs: Vec::new(),
            mutants: self
                .mutants(&repo)?
                .into_iter()
                .filter(|m| m.killed_by.iter().any(|k| names(k, name)))
                .collect(),
            accepts: self.accepts(&repo, name)?,
        };
        for run in runs {
            let (test, run) = run?;
            if names(&test, name) {
                detail.runs.push(run);
            }
        }
        if detail.runs.is_empty() && detail.mutants.is_empty() && detail.accepts.is_empty() {
            return Err(crate::error::coded(
                crate::error::Code::NotFound,
                format!("nothing is recorded for test {name}; see legionctl code tests list"),
            ));
        }
        Ok(detail)
    }
}
