//! Tests as evidence (design 23, A7): test runs per test, mutant records (from `cargo-mutants` on the files of the
//! change set, and from the agents' own mutation checks), the review signals of the new and changed tests of a change
//! set, and the gate that `workspace checkpoint` runs before it commits. The records are in `workspace.db`, keyed by
//! the repository (its git common folder, so that every worktree of one repository shares them).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};
use blake2::{Blake2b, Digest as _, digest::consts::U16};
use rusqlite::params;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::now;
use crate::dossier::Dossiers;
use crate::workspace::{git, git_common_dir};

/// The tables of the test records at workspace.db v12 (the step from v11).
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

/// The step from workspace.db v13 to v14: each kill and each accept is keyed by the test's file, libtest name and
/// token hash. The records before have no file and no hash: they do not count, and the gate asks for evidence again.
macro_rules! keyed {
    () => {
        "-- killed: the JSON list of {test, file, hash}; a kill counts while the test has the same hash
     UPDATE mutants SET killed = (SELECT json_group_array(json_object('test', value, 'file', NULL, 'hash', NULL))
                                  FROM json_each(mutants.killed));
     ALTER TABLE test_accepts ADD COLUMN file TEXT;
     ALTER TABLE test_accepts ADD COLUMN hash TEXT;"
    };
}
pub(crate) use keyed;

/// The command of `code tests run`; the filter goes after `--`. The JSON messages give each test binary's source.
const TEST_COMMAND: &str =
    "cargo test --release --workspace --no-fail-fast --message-format=json -j8";

/// A test that failed with a mutation, as it was then: it counts for the test while the test has the same hash.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct Kill {
    /// libtest's name: the module path and the function name.
    pub test: String,
    /// The test's file, relative to the repository (none: recorded before v14).
    pub file: Option<String>,
    /// The hash of the test function's tokens (none: recorded before v14).
    pub hash: Option<String>,
}

/// One mutation and the tests that failed with it (none: the mutant survived).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Mutant {
    pub file: String,
    pub line: u32,
    pub description: String,
    pub killed_by: Vec<Kill>,
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
    /// The test's libtest name, file and token hash: the accept counts while the test has the same hash.
    pub test: String,
    pub file: Option<String>,
    pub hash: Option<String>,
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
    /// libtest's name: the module path and the function name.
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

/// A test function: its file, its libtest name (module path and function name), the lines from its first attribute
/// to its closing brace (1-based), the hash of its tokens and its assertions.
struct TestFn {
    file: String,
    name: String,
    start: usize,
    end: usize,
    hash: String,
    assertions: u32,
}

impl TestFn {
    /// A record of this test as it is now: a kill counts only while the test has these tokens.
    fn kill(&self) -> Kill {
        Kill {
            test: self.name.clone(),
            file: Some(self.file.clone()),
            hash: Some(self.hash.clone()),
        }
    }
}

/// The module path of a Rust file in its crate: `src/a/b.rs` is `a::b`, a crate root (`src/lib.rs`, `src/main.rs`,
/// `src/bin/x.rs`, `tests/x.rs`) has none.
// shortcut: `#[path]` attributes are not read; read them when a repository uses them for test modules.
fn module_of(file: &str) -> Vec<String> {
    let parts: Vec<&str> = file
        .strip_suffix(".rs")
        .unwrap_or(file)
        .split('/')
        .collect();
    let dirs = &parts[..parts.len() - 1];
    let Some(root) = dirs.iter().rposition(|p| *p == "src").or_else(|| {
        dirs.iter()
            .rposition(|p| ["tests", "benches", "examples"].contains(p))
    }) else {
        return Vec::new();
    };
    let mut module = parts[root + 1..].to_vec();
    let crate_root = match module[..] {
        [one] => parts[root] != "src" || one == "lib" || one == "main",
        ["bin", _] => parts[root] == "src",
        _ => false,
    };
    if crate_root {
        return Vec::new();
    }
    if module.last() == Some(&"mod") {
        module.pop();
    }
    module.into_iter().map(String::from).collect()
}

/// An attribute that makes a function a test: its path ends in `test` (`test`, `tokio::test`, `sqlx::test`,
/// `rstest`) or is `test_case`.
fn is_test(attr: &syn::Attribute) -> bool {
    attr.path().segments.last().is_some_and(|s| {
        let name = s.ident.to_string();
        name.ends_with("test") || name == "test_case"
    })
}

/// The assertions in a test: the `assert*!` and `debug_assert*!` macro calls and `.unwrap_err()` calls of its code
/// (not of its comments or strings).
struct Assertions(u32);

impl<'a> syn::visit::Visit<'a> for Assertions {
    fn visit_macro(&mut self, m: &'a syn::Macro) {
        if m.path.segments.last().is_some_and(|s| {
            let name = s.ident.to_string();
            ["assert", "assert_eq", "assert_ne", "assert_matches"].contains(&name.as_str())
                || name.starts_with("debug_assert")
        }) {
            self.0 += 1;
        }
        syn::visit::visit_macro(self, m);
    }

    fn visit_expr_method_call(&mut self, call: &'a syn::ExprMethodCall) {
        if call.method == "unwrap_err" {
            self.0 += 1;
        }
        syn::visit::visit_expr_method_call(self, call);
    }
}

fn walk(file: &str, items: &[syn::Item], module: &mut Vec<String>, found: &mut Vec<TestFn>) {
    use quote::ToTokens as _;
    use syn::spanned::Spanned as _;
    use syn::visit::Visit as _;
    for item in items {
        match item {
            syn::Item::Fn(f) if f.attrs.iter().any(is_test) => {
                let mut count = Assertions(0);
                count.visit_block(&f.block);
                let should_panic = f.attrs.iter().any(|a| a.path().is_ident("should_panic"));
                // line numbers outside a proc macro: the span-locations feature of proc-macro2
                let span: proc_macro2::Span = f.span();
                let tokens = f.to_token_stream().to_string();
                let name = module
                    .iter()
                    .cloned()
                    .chain([f.sig.ident.to_string()])
                    .collect::<Vec<_>>()
                    .join("::");
                found.push(TestFn {
                    file: file.into(),
                    name,
                    start: span.start().line,
                    end: span.end().line,
                    hash: format!("{:x}", Blake2b::<U16>::digest(tokens.as_bytes())),
                    assertions: count.0 + u32::from(should_panic),
                });
            }
            syn::Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    module.push(m.ident.to_string());
                    walk(file, inner, module, found);
                    module.pop();
                }
            }
            _ => {}
        }
    }
}

/// The test functions of the Rust file `file` (relative to the repository) with the text `text`.
fn tests_in(file: &str, text: &str) -> Result<Vec<TestFn>> {
    let parsed = syn::parse_file(text).with_context(|| format!("parsing {file}"))?;
    let mut found = Vec::new();
    walk(file, &parsed.items, &mut module_of(file), &mut found);
    Ok(found)
}

/// Every test function in the working tree of the repository at `top` (its tracked and untracked Rust files), and
/// the files that do not parse.
fn worktree_tests(top: &Path) -> Result<(Vec<TestFn>, Vec<String>)> {
    let (mut tests, mut broken) = (Vec::new(), Vec::new());
    let files = git(
        top,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "*.rs",
        ],
    )?;
    for file in files.lines().collect::<BTreeSet<_>>() {
        // a tracked file that the working tree deleted has no tests
        let Ok(text) = std::fs::read_to_string(top.join(file)) else {
            continue;
        };
        match tests_in(file, &text) {
            Ok(found) => tests.extend(found),
            Err(_) => broken.push(file.to_string()),
        }
    }
    Ok((tests, broken))
}

/// The one test that `name` names: `<file>::<libtest name>`, a libtest name, or its end (a function name); an exact
/// libtest name comes before an end.
fn resolve<'a>(tests: &'a [TestFn], broken: &[String], name: &str) -> Result<&'a TestFn> {
    let (file, path) = match name.split_once(".rs::") {
        Some((file, path)) => (Some(format!("{file}.rs")), path),
        None => (None, name),
    };
    let of_file = |t: &&TestFn| file.as_ref().is_none_or(|f| &t.file == f);
    let mut found: Vec<&TestFn> = tests
        .iter()
        .filter(of_file)
        .filter(|t| t.name == path)
        .collect();
    if found.is_empty() {
        let end = format!("::{path}");
        found = tests
            .iter()
            .filter(of_file)
            .filter(|t| t.name.ends_with(&end))
            .collect();
    }
    match found[..] {
        [one] => Ok(one),
        [] => Err(crate::error::coded(
            crate::error::Code::NotFound,
            format!(
                "no test function {name} in the working tree{}",
                if broken.is_empty() {
                    String::new()
                } else {
                    format!(" (these files do not parse: {})", broken.join(", "))
                }
            ),
        )),
        _ => bail!(
            "{name} names {} tests: {}; give <file>::<name>",
            found.len(),
            found
                .iter()
                .map(|t| format!("{}::{}", t.file, t.name))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// A recorded test name (libtest's, with its module path) is the test function `name`.
fn names(recorded: &str, name: &str) -> bool {
    recorded == name || recorded.ends_with(&format!("::{name}"))
}

/// git in `dir`, with `index` as its index file when given (the `GIT_INDEX_FILE` of a running `git commit`).
fn git_with(dir: &Path, index: Option<&Path>, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }
    let out = cmd.output().context("running git")?;
    ensure!(
        out.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Where a change set is: the working tree, or an index (the repository's own, or a `git commit`'s other file).
#[derive(Clone, Copy)]
pub enum Side<'a> {
    WorkTree,
    Index(Option<&'a Path>),
}

/// The changed Rust files of the repository at `top` against `base` (with the untracked files in the working tree)
/// and the new-side lines each change touches.
fn change_set(top: &Path, base: &str, side: Side) -> Result<BTreeMap<String, Vec<(usize, usize)>>> {
    let mut args = vec!["diff", "-U0", "--no-color", "--no-ext-diff"];
    let index = match side {
        Side::Index(index) => {
            args.push("--cached");
            index
        }
        Side::WorkTree => None,
    };
    args.extend([base, "--", "*.rs"]);
    let mut files: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    let mut file = None;
    for line in git_with(top, index, &args)?.lines() {
        if let Some(path) = line.strip_prefix("+++ ") {
            file = path.strip_prefix("b/").map(String::from);
        } else if let (Some(hunk), Some(f)) = (line.strip_prefix("@@ "), &file) {
            // `@@ -a,b +c,d @@`: new lines c..c+d; d = 0 is a removal after line c, and touches no new line
            let new = hunk
                .split(' ')
                .nth(1)
                .unwrap_or_default()
                .trim_start_matches('+');
            let (start, count) = new.split_once(',').unwrap_or((new, "1"));
            let start: usize = start.parse()?;
            let count: usize = count.parse()?;
            if count > 0 {
                files
                    .entry(f.clone())
                    .or_default()
                    .push((start, start + count - 1));
            }
        }
    }
    if matches!(side, Side::WorkTree) {
        for path in git(
            top,
            &["ls-files", "--others", "--exclude-standard", "--", "*.rs"],
        )?
        .lines()
        {
            files.insert(path.to_string(), vec![(1, usize::MAX)]);
        }
    }
    Ok(files)
}

/// The text of `path` (relative to the repository at `top`) in the change set.
fn content(top: &Path, path: &str, side: Side) -> Result<String> {
    match side {
        Side::WorkTree => {
            std::fs::read_to_string(top.join(path)).with_context(|| format!("reading {path}"))
        }
        Side::Index(index) => git_with(top, index, &["show", &format!(":{path}")]),
    }
}

/// The top folder of the repository or worktree that contains `dir`.
fn top(dir: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(git(dir, &["rev-parse", "--show-toplevel"])?))
}

/// The mutants of a cargo-mutants run in `results` (its `mutants.out`), each with the tests of `tests` that failed in
/// its log; and a line for each failed test name that names no one test.
fn tool_mutants(
    results: &Path,
    (tests, broken): &(Vec<TestFn>, Vec<String>),
) -> Result<(Vec<Mutant>, Vec<String>)> {
    let path = results.join("outcomes.json");
    let o: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)
        .with_context(|| format!("reading {}", path.display()))?;
    let (mut found, mut unknown) = (Vec::new(), Vec::new());
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
        let mut killed_by = Vec::new();
        for name in log
            .lines()
            .filter_map(|l| l.strip_prefix("test ")?.strip_suffix(" ... FAILED"))
        {
            match resolve(tests, broken, name) {
                Ok(t) => killed_by.push(t.kill()),
                Err(e) => unknown.push(format!("  not recorded: {e:#}")),
            }
        }
        found.push(Mutant {
            file: mutant["file"].as_str().unwrap_or_default().into(),
            line: mutant["span"]["start"]["line"]
                .as_u64()
                .map_or(0, |l| u32::try_from(l).unwrap_or(0)),
            description: mutant["name"].as_str().unwrap_or_default().into(),
            killed_by,
            source: "tool".into(),
            git_ref: String::new(),
            at: 0,
        });
    }
    unknown.dedup();
    Ok((found, unknown))
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

    fn accepts(&self, repo: &str) -> Result<Vec<Accept>> {
        let mut stmt = self.db().prepare(
            "SELECT test, file, hash, reason, session, ts FROM test_accepts WHERE repo = ?1 ORDER BY ts, rowid",
        )?;
        let rows = stmt.query_map([repo], |r| {
            Ok(Accept {
                test: r.get(0)?,
                file: r.get(1)?,
                hash: r.get(2)?,
                reason: r.get(3)?,
                session: r.get(4)?,
                at: r.get(5)?,
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

    /// Record a mutation an agent made by hand and the tests that failed with it, each as it is in the working tree
    /// now (`<file>::<name>`, a libtest name, or a function name).
    ///
    /// # Errors
    /// `dir` is not in a git repository, a name names no one test, or the database write fails.
    pub fn record_mutant(
        &self,
        dir: &Path,
        file: &str,
        line: u32,
        description: &str,
        killed_by: &[String],
    ) -> Result<String> {
        ensure!(
            !description.trim().is_empty(),
            "a mutant needs a description of the mutation"
        );
        let found = worktree_tests(&top(dir)?)?;
        let killed_by = killed_by
            .iter()
            .map(|name| Ok(resolve(&found.0, &found.1, name)?.kill()))
            .collect::<Result<Vec<_>>>()?;
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
            let killers: Vec<String> = m
                .killed_by
                .iter()
                .map(|k| format!("{}::{}", k.file.as_deref().unwrap_or_default(), k.test))
                .collect();
            format!(
                "recorded {file}:{line} {description}, killed by {}",
                killers.join(", ")
            )
        })
    }

    /// Let `test` (as it is in the working tree now) through the gate without evidence, for `reason`; the test's
    /// `<file>::<libtest name>`.
    ///
    /// # Errors
    /// The reason is empty, `dir` is not in a git repository, `test` names no one test, or the database write fails.
    pub fn accept_test(
        &self,
        dir: &Path,
        test: &str,
        reason: &str,
        session: &str,
    ) -> Result<String> {
        ensure!(
            !reason.trim().is_empty(),
            "--accept-test needs --reason <why the test has value>"
        );
        let found = worktree_tests(&top(dir)?)?;
        let t = resolve(&found.0, &found.1, test)?;
        self.db().execute(
            "INSERT INTO test_accepts (repo, test, file, hash, reason, session, ts) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![repo_key(dir)?, t.name, t.file, t.hash, reason, session, now()],
        )?;
        Ok(format!("{}::{}", t.file, t.name))
    }

    /// The review of the new and changed tests of the change set against HEAD on `side`: assertions, kills,
    /// duplicates, place, and the hard signals the gate refuses. A kill or an accept counts only for the test with
    /// the file, name and token hash it was recorded with.
    ///
    /// # Errors
    /// git fails in `dir`, a changed Rust file does not parse, or the database read fails.
    pub fn review_tests(&self, dir: &Path, side: Side) -> Result<TestReview> {
        let (repo, top) = (repo_key(dir)?, top(dir)?);
        let mutants = self.mutants(&repo)?;
        let accepts = self.accepts(&repo)?;
        // every killer's set of mutants, for the kills and the duplicates
        let mut killed: BTreeMap<&Kill, BTreeSet<usize>> = BTreeMap::new();
        for (i, m) in mutants.iter().enumerate() {
            for k in &m.killed_by {
                killed.entry(k).or_default().insert(i);
            }
        }
        let mut tests = Vec::new();
        for (file, hunks) in change_set(&top, "HEAD", side)? {
            let text = content(&top, &file, side)?;
            for t in tests_in(&file, &text)? {
                if !hunks.iter().any(|&(a, b)| a <= t.end && t.start <= b) {
                    continue;
                }
                let key = t.kill();
                let own = killed.get(&key).cloned().unwrap_or_default();
                let duplicate_of = (!own.is_empty())
                    .then(|| {
                        killed
                            .iter()
                            .find(|(k, s)| {
                                (&k.test, &k.file) != (&key.test, &key.file) && own.is_subset(s)
                            })
                            .map(|(k, _)| k.test.clone())
                    })
                    .flatten();
                let boundary = Path::new(&file)
                    .components()
                    .any(|c| c.as_os_str() == "tests");
                let mut refused = Vec::new();
                if t.assertions == 0 {
                    refused.push(
                        "no assertion (assert*!, debug_assert*!, unwrap_err, #[should_panic])"
                            .into(),
                    );
                }
                if own.is_empty() {
                    refused.push(
                        "kills no mutant in this version: record a mutation it catches (code tests mutant record) or run code tests mutant run"
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
                let accepted = accepts
                    .iter()
                    .rev()
                    .find(|a| (&a.test, &a.file, &a.hash) == (&key.test, &key.file, &key.hash))
                    .map(|a| a.reason.clone());
                tests.push(ReviewedTest {
                    kills: u32::try_from(own.len()).unwrap_or(u32::MAX),
                    name: t.name,
                    file: file.clone(),
                    assertions: t.assertions,
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
        let refusals = self.review_tests(dir, Side::Index(None))?.refusals();
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

    /// Run the test suite in `dir` (`cargo test`, with `filter` after `--`) and record each test's result with the
    /// source of its test binary (relative to the repository); `progress` gets the counts of results so far.
    ///
    /// # Errors
    /// The command cannot start, it fails and no test ran (a build error: the error has the end of its output), or
    /// a database write fails.
    pub fn run_tests(
        &self,
        dir: &Path,
        filter: Option<&str>,
        job: i64,
        mut progress: impl FnMut(&BTreeMap<String, u32>),
    ) -> Result<Vec<String>> {
        let (repo, head, top) = (repo_key(dir)?, git(dir, &["rev-parse", "HEAD"])?, top(dir)?);
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
        // each test binary's file name -> its source; the last lines of text and compiler errors, for a failure
        let mut sources: BTreeMap<String, String> = BTreeMap::new();
        let mut tail: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        let mut file = String::new();
        for line in BufReader::new(child.stdout.take().context("no output")?).lines() {
            let line = line?;
            if let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line) {
                if let (Some(exe), Some(src)) = (
                    msg["executable"].as_str(),
                    msg["target"]["src_path"].as_str(),
                ) {
                    let src = Path::new(src);
                    sources.insert(
                        Path::new(exe)
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into(),
                        src.strip_prefix(&top)
                            .unwrap_or(src)
                            .to_string_lossy()
                            .into(),
                    );
                } else if let Some(text) = msg["message"]["rendered"].as_str() {
                    tail.push_back(text.trim_end().into());
                }
            } else if let Some(rest) = line.trim().strip_prefix("Running ") {
                // `Running tests/e2e.rs (target/release/deps/e2e-1f2e)`: the binary in parentheses
                let exe = rest
                    .rsplit_once(" (")
                    .map_or(rest, |(_, e)| e)
                    .trim_end_matches(')');
                let name = Path::new(exe)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy();
                file.clone_from(
                    sources.get(name.as_ref()).with_context(|| {
                        format!("cargo named no source for the test binary {exe}")
                    })?,
                );
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
            } else {
                tail.push_back(line);
            }
            while tail.len() > 40 {
                tail.pop_front();
            }
        }
        let status = child.wait()?;
        if !status.success() && counts.is_empty() {
            bail!(
                "{TEST_COMMAND} failed ({status}) and no test ran:\n{}",
                Vec::from(tail).join("\n")
            );
        }
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
        let top = top(dir)?;
        let files: Vec<String> = change_set(&top, &from, Side::WorkTree)?
            .into_keys()
            .filter(|f| !Path::new(f).components().any(|c| c.as_os_str() == "tests"))
            .filter(|f| top.join(f).is_file())
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
        let (mutants, unknown) = tool_mutants(&results, &worktree_tests(&top)?)?;
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
        lines.extend(unknown);
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
            "SELECT test, file, result, ref, MAX(ts) FROM test_runs WHERE repo = ?1 GROUP BY file, test ORDER BY file, test",
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
                .filter(|m| m.killed_by.iter().any(|k| k.test == row.name))
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
                .filter(|m| m.killed_by.iter().any(|k| names(&k.test, name)))
                .collect(),
            accepts: self
                .accepts(&repo)?
                .into_iter()
                .filter(|a| names(&a.test, name))
                .collect(),
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
