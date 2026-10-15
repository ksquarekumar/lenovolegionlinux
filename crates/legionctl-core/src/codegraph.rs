//! Code graphs (design 23, Part A, P1-P2): one ctx graph snapshot per git tree hash in
//! `<main checkout>/.graf/refs/<tree>.db`, which every worktree of the repository shares, and the change graph
//! between refs by stable symbol keys. ctx parses the code; this module does refs, prune, diff and annotations.
//!
//! A snapshot is immutable (read-only file, never updated): it is the content address of its tree. Each checkout's
//! `.graf/index.db` is a link to the snapshot of its HEAD, so `ctx mcp serve` (which opens the nearest
//! `.graf/index.db`) reads the code of the checkout's HEAD.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use blake2::{Blake2b, Digest, digest::consts::U16};
use rusqlite::{OpenFlags, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::db::{Db, EventFilter};
use crate::dossier::Dossiers;
use crate::error::{Code, coded};
use crate::workspace::{git_common_dir, main_checkout};

/// Snapshots kept besides the heads, bases and merge-bases (design decision 6).
const KEEP: usize = 20;
/// The line in `<common dir>/info/exclude` that keeps `.graf/` out of every worktree's `git status`.
const IGNORE: &str = "/.graf/";
/// Marks the lines that `hook install` adds to a git hook.
const MARK: &str = "# legionctl code graph";
const HOOKS: [&str; 4] = ["post-commit", "post-checkout", "post-merge", "post-rewrite"];
/// The event that links a commit (its patch id) to the checkpoint that made it.
pub const COMMIT_EVENT: &str = "workspace.checkpoint.commit";

/// A name part that no other build of this process or of another process has: the process, the time and a count.
fn unique() -> String {
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}.{nanos}.{n}", std::process::id())
}

pub(crate) fn run(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("running git")?;
    ensure!(
        out.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(out.stdout)
}

pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&run(dir, args)?).trim().to_string())
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Blake2b::<U16>::digest(bytes))
}

/// The `.graf` folder of the repository that contains `dir`: in its main checkout, for every worktree.
///
/// # Errors
/// `dir` is not in a git repository.
pub fn graf(dir: &Path) -> Result<PathBuf> {
    Ok(main_checkout(dir)?.join(".graf"))
}

/// The snapshot file of a tree.
fn file(graf: &Path, tree: &str) -> PathBuf {
    graf.join("refs").join(format!("{tree}.db"))
}

/// A ref resolved: the commit and its tree.
pub(crate) fn resolve(dir: &Path, rev: &str) -> Result<(String, String)> {
    ensure!(!rev.starts_with('-'), "a ref must not start with '-'");
    let commit = git(
        dir,
        &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
    )
    .map_err(|_| {
        coded(
            Code::NotFound,
            format!("no commit '{rev}' in {}", dir.display()),
        )
    })?;
    let tree = git(dir, &["rev-parse", &format!("{commit}^{{tree}}")])?;
    Ok((commit, tree))
}

/// Whether the repository of `dir` keeps snapshots: its `.graf/refs` exists (the first `code graph refresh` or
/// `code graph hook install` makes it). The triggers (checkpoint, merge, session start) act only then.
#[must_use]
pub fn enabled(dir: &Path) -> bool {
    graf(dir).is_ok_and(|g| g.join("refs").is_dir())
}

/// Add `/.graf/` to the repository's `info/exclude` (shared by its worktrees) when git does not ignore it yet.
fn exclude(dir: &Path) -> Result<()> {
    if run(
        dir,
        &["check-ignore", "--quiet", "--no-index", ".graf/index.db"],
    )
    .is_ok()
    {
        return Ok(());
    }
    let info = git_common_dir(dir)?.join("info");
    std::fs::create_dir_all(&info)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(info.join("exclude"))?;
    writeln!(file, "{IGNORE}")?;
    Ok(())
}

/// The snapshot of `rev`, made when it is missing: `git archive` of its tree into a new folder, `ctx graph index`
/// into a temporary file, then a hard link to `<tree>.db` (the first builder wins; a second one removes its file).
/// Returns the tree and whether this call made it.
///
/// # Errors
/// The ref does not resolve, ctx is missing or refuses the tree, or a file cannot be written.
pub fn snapshot(dir: &Path, rev: &str) -> Result<(String, bool)> {
    let (_, tree) = resolve(dir, rev)?;
    let graf = graf(dir)?;
    let target = file(&graf, &tree);
    if target.exists() {
        return Ok((tree, false));
    }
    let bin = crate::setup::ctx_bin()
        .context("ctx is not installed; run `legionctl setup apply --install-ctx`")?;
    std::fs::create_dir_all(graf.join("refs"))?;
    exclude(dir)?;
    let suffix = unique();
    let source = graf.join(format!("src.{tree}.{suffix}"));
    let tmp = graf.join("refs").join(format!("{tree}.db.tmp.{suffix}"));
    let built = (|| {
        std::fs::create_dir_all(&source)?;
        let mut archive = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["archive", "--format=tar", &tree])
            .stdout(Stdio::piped())
            .spawn()
            .context("running git archive")?;
        let untar = Command::new("tar")
            .arg("-x")
            .arg("-C")
            .arg(&source)
            .stdin(archive.stdout.take().context("no archive output")?)
            .status()
            .context("running tar")?;
        ensure!(
            archive.wait()?.success() && untar.success(),
            "git archive of {tree} failed"
        );
        let out = Command::new(&bin)
            .args(["graph", "index"])
            .arg(&source)
            .arg("--db")
            .arg(&tmp)
            .args(["--code-only", "--json"])
            .output()
            .context("running ctx graph index")?;
        ensure!(
            out.status.success(),
            "ctx graph index refused tree {tree}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        // read-only: a `ctx graph index` without --db through the index.db link cannot change a snapshot
        std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o444))?;
        match std::fs::hard_link(&tmp, &target) {
            Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => Err(e.into()),
            linked => Ok(linked.is_ok()),
        }
    })();
    let _ = std::fs::remove_dir_all(&source);
    let _ = std::fs::remove_file(&tmp);
    Ok((tree, built?))
}

/// The snapshot of `rev` that a read uses: made when `build`, else a `NotFound` error that says how to make it.
pub(crate) fn snapshot_for(dir: &Path, rev: &str, build: bool) -> Result<(String, PathBuf)> {
    let tree = if build {
        snapshot(dir, rev)?.0
    } else {
        resolve(dir, rev)?.1
    };
    let path = file(&graf(dir)?, &tree);
    if !path.exists() {
        return Err(coded(
            Code::NotFound,
            format!(
                "no graph for {rev} (tree {tree}); add --build, or run `legionctl code graph refresh --ref {rev}`"
            ),
        ));
    }
    Ok((tree, path))
}

/// Point the checkout's `.graf/index.db` (what `ctx mcp serve` opens) at the snapshot of its HEAD.
fn link(dir: &Path, tree: &str) -> Result<()> {
    let top = PathBuf::from(git(dir, &["rev-parse", "--show-toplevel"])?);
    let own = top.join(".graf");
    std::fs::create_dir_all(&own)?;
    let tmp = own.join(format!("index.db.{}", unique()));
    std::os::unix::fs::symlink(file(&graf(dir)?, tree), &tmp)?;
    std::fs::rename(&tmp, own.join("index.db"))?;
    Ok(())
}

/// Make the snapshot of `rev`, link the checkout's `.graf/index.db` to it when `rev` is its HEAD,
/// then prune.
///
/// # Errors
/// See [`snapshot`] and [`prune`].
pub fn refresh(dossiers: &Dossiers, dir: &Path, rev: &str) -> Result<Vec<String>> {
    let (tree, built) = snapshot(dir, rev)?;
    let mut lines = vec![format!(
        "{rev}: graph {tree} {}",
        if built { "made" } else { "was there" }
    )];
    if resolve(dir, "HEAD")?.1 == tree {
        link(dir, &tree)?;
    }
    lines.extend(prune(dossiers, dir, false)?);
    Ok(lines)
}

/// The trees that prune keeps whatever their age, each with why: branch heads, worktree HEADs, and for each open
/// workspace of the repository its base and the merge-base of base and branch.
fn kept(dossiers: &Dossiers, dir: &Path) -> Result<BTreeMap<String, Vec<String>>> {
    let mut keep: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in git(
        dir,
        &[
            "for-each-ref",
            "--format=%(tree) %(refname:short)",
            "refs/heads",
        ],
    )?
    .lines()
    {
        if let Some((tree, name)) = line.split_once(' ') {
            keep.entry(tree.into()).or_default().push(name.into());
        }
    }
    let mut checkout = None;
    for line in git(dir, &["worktree", "list", "--porcelain"])?.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            checkout = Some(path.to_string());
        } else if let (Some(commit), Some(path)) = (line.strip_prefix("HEAD "), &checkout) {
            let tree = git(dir, &["rev-parse", &format!("{commit}^{{tree}}")])?;
            keep.entry(tree)
                .or_default()
                .push(format!("HEAD of {path}"));
        }
    }
    let main = main_checkout(dir)?;
    for ws in dossiers.workspaces(None)? {
        if ws.repo != main || ws.is_folder() {
            continue;
        }
        if let Ok((_, tree)) = resolve(dir, &ws.base) {
            keep.entry(tree)
                .or_default()
                .push(format!("base of {}", ws.branch));
        }
        if let Ok(base) = git(dir, &["merge-base", &ws.base, &ws.branch]) {
            let (_, tree) = resolve(dir, &base)?;
            keep.entry(tree)
                .or_default()
                .push(format!("merge-base of {}", ws.branch));
        }
    }
    Ok(keep)
}

/// One snapshot file: its tree, size and change time.
fn snapshots(graf: &Path) -> Result<Vec<(String, u64, SystemTime)>> {
    let Ok(dir) = std::fs::read_dir(graf.join("refs")) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in dir {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(tree) = name.strip_suffix(".db") {
            let meta = entry.metadata()?;
            out.push((tree.to_string(), meta.len(), meta.modified()?));
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.2));
    Ok(out)
}

/// Remove the snapshots that no head, base or merge-base needs, except the newest 20, and temporary files older
/// than one hour (a build that a restart cut off).
///
/// # Errors
/// git fails, or a file cannot be removed.
pub fn prune(dossiers: &Dossiers, dir: &Path, dry_run: bool) -> Result<Vec<String>> {
    let graf = graf(dir)?;
    let keep = kept(dossiers, dir)?;
    let mut lines = Vec::new();
    let mut others = 0;
    for (tree, _, _) in snapshots(&graf)? {
        if keep.contains_key(&tree) {
            continue;
        }
        others += 1;
        if others > KEEP {
            if !dry_run {
                std::fs::remove_file(file(&graf, &tree))?;
            }
            lines.push(format!("removed graph {tree}"));
        }
    }
    let hour_ago = SystemTime::now() - Duration::from_secs(3600);
    for entry in std::fs::read_dir(&graf)?.chain(std::fs::read_dir(graf.join("refs"))?) {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if (name.starts_with("src.") || name.contains(".db.tmp."))
            && entry.metadata()?.modified()? < hour_ago
        {
            if !dry_run {
                let _ = std::fs::remove_dir_all(entry.path());
                let _ = std::fs::remove_file(entry.path());
            }
            lines.push(format!("removed left-over {name}"));
        }
    }
    Ok(lines)
}

/// A ref of the repository and whether it has a graph.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphRef {
    pub name: String,
    pub commit: String,
    pub tree: String,
    /// Its snapshot exists ("has .graf").
    pub graph: bool,
}

/// One snapshot file.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphSnapshot {
    pub tree: String,
    pub bytes: u64,
    pub modified: i64,
    /// Why prune keeps it whatever its age: the heads, bases and merge-bases on this tree.
    pub refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphRefs {
    /// HEAD and the branches.
    pub refs: Vec<GraphRef>,
    /// Newest first.
    pub snapshots: Vec<GraphSnapshot>,
}

/// HEAD and each branch with its tree and whether it has a graph, and each snapshot with the refs on it.
///
/// # Errors
/// `dir` is not in a git repository.
pub fn refs(dossiers: &Dossiers, dir: &Path) -> Result<GraphRefs> {
    let graf = graf(dir)?;
    let mut refs = Vec::new();
    let heads = git(
        dir,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )?;
    for name in std::iter::once("HEAD").chain(heads.lines()) {
        let (commit, tree) = resolve(dir, name)?;
        refs.push(GraphRef {
            graph: file(&graf, &tree).exists(),
            name: name.into(),
            commit,
            tree,
        });
    }
    let mut keep = kept(dossiers, dir)?;
    let snapshots = snapshots(&graf)?
        .into_iter()
        .map(|(tree, bytes, modified)| GraphSnapshot {
            refs: keep.remove(&tree).unwrap_or_default(),
            modified: modified
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX)),
            tree,
            bytes,
        })
        .collect();
    Ok(GraphRefs { refs, snapshots })
}

/// One ctx node with its stable key.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CodeNode {
    /// The stable key: the same symbol has it at each ref (`rust:<crate dir>:<module path>::<name>`, `file:<path>`).
    pub key: String,
    /// ctx's node id at this tree (it holds byte offsets).
    pub id: String,
    pub kind: String,
    pub file: String,
    pub line: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CodeNodes {
    pub tree: String,
    pub nodes: Vec<CodeNode>,
}

/// A node of a snapshot, as the diff needs it.
#[derive(Debug, Clone)]
pub(crate) struct Node {
    id: String,
    pub(crate) label: String,
    pub(crate) kind: String,
    pub(crate) file: String,
    line: Option<i64>,
    end_line: Option<i64>,
    span: Option<(usize, usize)>,
}

impl Node {
    /// Its lines from the first to the last (with blanks and comments).
    pub(crate) fn span_lines(&self) -> i64 {
        self.end_line.unwrap_or(0) - self.line.unwrap_or(0) + 1
    }
}

/// `id` without its `@<byte offset>` parts.
fn without_offsets(id: &str) -> String {
    let mut out = String::new();
    let mut chars = id.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '@' && chars.peek().is_some_and(char::is_ascii_digit) {
            while chars.next_if(char::is_ascii_digit).is_some() {}
        } else {
            out.push(c);
        }
    }
    out
}

/// The key part of a node: ctx's `binding_key` (`rust:<crate dir>:<module path>::<name>`, also for private items),
/// else its first binding alias that is neither a package alias nor a bare `symbol:<name>` (closures), else its id;
/// each without its `@<byte offset>` parts (closures and blocks carry them).
fn base_key(id: &str, binding_key: Option<String>, metadata: &Value) -> String {
    let alias = || {
        metadata["binding_aliases"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .find(|a| {
                a.split(':').nth(1) != Some("package")
                    && (!a.starts_with("symbol:") || a.contains("::"))
            })
            .map(String::from)
    };
    without_offsets(&binding_key.or_else(alias).unwrap_or_else(|| id.into()))
}

/// The nodes of a snapshot by stable key; a second node with the same key in file order gets `#2`, and so on.
pub(crate) fn nodes(db: &Path) -> Result<BTreeMap<String, Node>> {
    let conn = rusqlite::Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare(
        "SELECT id, kind, file, line, end_line, metadata, binding_key, label FROM nodes",
    )?;
    let mut rows: Vec<(String, Node)> = stmt
        .query_map([], |r| {
            let metadata: Value = serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default();
            let id: String = r.get(0)?;
            let span = metadata["start_byte"]
                .as_u64()
                .zip(metadata["end_byte"].as_u64())
                .and_then(|(s, e)| Some((usize::try_from(s).ok()?, usize::try_from(e).ok()?)));
            Ok((
                base_key(&id, r.get(6)?, &metadata),
                Node {
                    id,
                    label: r.get(7)?,
                    kind: r.get(1)?,
                    file: r.get(2)?,
                    line: r.get(3)?,
                    end_line: r.get(4)?,
                    span,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    rows.sort_by(|a, b| (&a.1.file, a.1.span).cmp(&(&b.1.file, b.1.span)));
    let mut out = BTreeMap::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (key, node) in rows {
        let n = seen.entry(key.clone()).or_insert(0);
        *n += 1;
        let key = if *n == 1 { key } else { format!("{key}#{n}") };
        out.insert(key, node);
    }
    Ok(out)
}

/// Blobs of one tree, read once each.
struct Blobs<'a> {
    dir: &'a Path,
    tree: &'a str,
    read: HashMap<String, Vec<u8>>,
}

impl Blobs<'_> {
    /// The node's bytes in its blob at this tree (the whole blob for a node without a range), and whether that is
    /// the whole blob.
    fn bytes(&mut self, node: &Node) -> Result<(&[u8], bool)> {
        if !self.read.contains_key(&node.file) {
            let blob = run(
                self.dir,
                &["cat-file", "blob", &format!("{}:{}", self.tree, node.file)],
            )?;
            self.read.insert(node.file.clone(), blob);
        }
        let blob = &self.read[&node.file];
        let part = node.span.and_then(|(s, e)| blob.get(s..e)).unwrap_or(blob);
        Ok((part, part.len() == blob.len()))
    }

    /// BLAKE2b-128 of the node's bytes at this tree.
    fn content_hash(&mut self, node: &Node) -> Result<String> {
        Ok(hash(self.bytes(node)?.0))
    }
}

/// Symbols whose label matches `text` at `rev`, by ctx (`ctx graph search --db`).
///
/// # Errors
/// No graph for the ref (without `build`), or ctx fails.
pub fn query(dir: &Path, text: &str, rev: &str, build: bool) -> Result<CodeNodes> {
    let (tree, db) = snapshot_for(dir, rev, build)?;
    let out = ctx(dir, &["graph", "search", text, "--json", "--db"], &db)?;
    Ok(CodeNodes {
        nodes: listed(&db, &out["nodes"])?,
        tree,
    })
}

/// What depends on the symbol with this stable key at `rev`: the key mapped to ctx's node id of that tree, then
/// `ctx graph impact --db`.
///
/// # Errors
/// No graph for the ref (without `build`), no such key at the ref, or ctx fails.
pub fn impact(dir: &Path, key: &str, rev: &str, depth: u8, build: bool) -> Result<CodeNodes> {
    let (tree, db) = snapshot_for(dir, rev, build)?;
    let node = nodes(&db)?
        .remove(key)
        .ok_or_else(|| coded(Code::NotFound, format!("no symbol {key} at {rev}")))?;
    let depth = depth.to_string();
    let out = ctx(
        dir,
        &[
            "graph", "impact", &node.id, "--depth", &depth, "--json", "--db",
        ],
        &db,
    )?;
    Ok(CodeNodes {
        nodes: listed(&db, &out["graph"]["nodes"])?,
        tree,
    })
}

/// ctx with these arguments and the snapshot as the last one, its JSON output.
fn ctx(dir: &Path, args: &[&str], db: &Path) -> Result<Value> {
    let bin = crate::setup::ctx_bin()
        .context("ctx is not installed; run `legionctl setup apply --install-ctx`")?;
    let out = Command::new(bin)
        .current_dir(dir)
        .args(args)
        .arg(db)
        .output()
        .context("running ctx")?;
    ensure!(
        out.status.success(),
        "ctx {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(serde_json::from_slice(&out.stdout)?)
}

/// ctx's nodes in a JSON list, each with its stable key in this snapshot.
fn listed(db: &Path, list: &Value) -> Result<Vec<CodeNode>> {
    let by_id: HashMap<String, String> =
        nodes(db)?.into_iter().map(|(key, n)| (n.id, key)).collect();
    Ok(list
        .as_array()
        .into_iter()
        .flatten()
        .map(|n| {
            let id = n["id"].as_str().unwrap_or_default().to_string();
            CodeNode {
                key: by_id.get(&id).cloned().unwrap_or_default(),
                kind: n["kind"].as_str().unwrap_or_default().into(),
                file: n["file"].as_str().unwrap_or_default().into(),
                line: n["line"].as_i64(),
                id,
            }
        })
        .collect())
}

/// One entity that changed between two refs.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EntityChange {
    pub key: String,
    pub kind: String,
    pub file: String,
    /// added, removed, modified (other content) or moved (same content, other file). A rename is removed plus added.
    pub change: String,
    /// `+a -r` lines for a file; the span in lines before and after for a symbol.
    pub lines: String,
    /// The commits of the range that touched its lines (its file, for a removed entity).
    pub commits: Vec<String>,
}

/// One commit of the range, with what made it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CommitRow {
    pub commit: String,
    pub time: i64,
    pub author: String,
    pub subject: String,
    /// The plan rows that the message names (`todo-NN`).
    pub plan_rows: Vec<String>,
    /// The `workspace.checkpoint` event with this message, and its session.
    pub checkpoint: Option<i64>,
    pub session: Option<String>,
    /// `ctx blame commit` (the agent session that made it), or its error.
    pub provenance: Value,
}

/// The changes from one ref to the next.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ChangeSet {
    /// BLAKE2b-128 of `<from tree>..<to tree>`.
    pub id: String,
    pub from: String,
    pub from_tree: String,
    pub to: String,
    pub to_tree: String,
    pub entities: Vec<EntityChange>,
    pub commits: Vec<CommitRow>,
    /// The text lists every entity; else it folds the entities that are not code into counts per file.
    pub detail: bool,
    /// Three-way, in the set base to theirs: the keys that base to ours changed too (the conflict candidates).
    pub both: Option<Vec<String>>,
}

/// The change sets between consecutive refs (`a b c`: a to b, b to c); one ref: from its first parent.
///
/// # Errors
/// A ref does not resolve, a ref has no graph (without `build`), or git fails.
pub fn changes(dir: &Path, revs: &[String], build: bool) -> Result<Vec<ChangeSet>> {
    let revs: Vec<String> = match revs {
        [] => vec!["HEAD^".into(), "HEAD".into()],
        [one] => vec![format!("{one}^"), one.clone()],
        many => many.to_vec(),
    };
    revs.windows(2)
        .map(|pair| change_set(dir, &pair[0], &pair[1], build))
        .collect()
}

fn change_set(dir: &Path, from: &str, to: &str, build: bool) -> Result<ChangeSet> {
    let (from_tree, from_db) = snapshot_for(dir, from, build)?;
    let (to_tree, to_db) = snapshot_for(dir, to, build)?;
    let (from_commit, to_commit) = (resolve(dir, from)?.0, resolve(dir, to)?.0);
    let range = format!("{from_commit}..{to_commit}");
    let mut numstat: BTreeMap<String, String> = BTreeMap::new();
    for line in git(
        dir,
        &["diff", "--numstat", "--no-renames", &from_tree, &to_tree],
    )?
    .lines()
    {
        let mut parts = line.splitn(3, '\t');
        if let (Some(a), Some(r), Some(path)) = (parts.next(), parts.next(), parts.next()) {
            numstat.insert(path.into(), format!("+{a} -{r}"));
        }
    }
    let (before, after) = (nodes(&from_db)?, nodes(&to_db)?);
    let mut old = Blobs {
        dir,
        tree: &from_tree,
        read: HashMap::new(),
    };
    let mut new = Blobs {
        dir,
        tree: &to_tree,
        read: HashMap::new(),
    };
    let mut entities = Vec::new();
    let keys: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    for key in keys {
        let row = |node: &Node, change: &str, lines: String| EntityChange {
            key: key.clone(),
            kind: node.kind.clone(),
            file: node.file.clone(),
            change: change.into(),
            lines,
            commits: Vec::new(),
        };
        let lines_of = |node: &Node, symbol: String| {
            if key.starts_with("file:") {
                numstat.get(&node.file).cloned().unwrap_or_default()
            } else {
                symbol
            }
        };
        match (before.get(key), after.get(key)) {
            (None, Some(b)) => {
                entities.push(row(b, "added", lines_of(b, format!("+{}", b.span_lines()))));
            }
            (Some(a), None) => {
                entities.push(row(
                    a,
                    "removed",
                    lines_of(a, format!("-{}", a.span_lines())),
                ));
            }
            (Some(a), Some(b)) => {
                if a.file == b.file && !numstat.contains_key(&a.file) {
                    continue;
                }
                let (ha, hb) = (old.content_hash(a)?, new.content_hash(b)?);
                let change = if ha != hb {
                    "modified"
                } else if a.file != b.file {
                    "moved"
                } else {
                    continue;
                };
                entities.push(row(
                    b,
                    change,
                    lines_of(b, format!("{} -> {}", a.span_lines(), b.span_lines())),
                ));
            }
            (None, None) => {}
        }
    }
    file_rows(dir, (&from_tree, &to_tree), &numstat, &mut entities);
    link_commits(
        dir,
        &range,
        (&before, &after),
        (&mut old, &mut new),
        &mut entities,
    )?;
    Ok(ChangeSet {
        id: hash(format!("{from_tree}..{to_tree}").as_bytes()),
        from: from.into(),
        to: to.into(),
        commits: commits(dir, &range)?,
        from_tree,
        to_tree,
        entities,
        detail: false,
        both: None,
    })
}

/// A three-way compare for a rebase or a merge: base to ours, and base to theirs with the keys that both sides
/// changed. A file or module row is no candidate when each side changed a narrower entity of its file: those entities
/// tell what changed, and other changes in the file are position only. When one side changed only the file's other
/// lines, the row stays a candidate.
///
/// # Errors
/// A ref does not resolve, a ref has no graph (without `build`), or git fails.
pub fn three_way(
    dir: &Path,
    [base, ours, theirs]: [&str; 3],
    build: bool,
) -> Result<Vec<ChangeSet>> {
    let ours = change_set(dir, base, ours, build)?;
    let mut theirs = change_set(dir, base, theirs, build)?;
    let container = |e: &EntityChange| e.kind == "file" || e.kind == "module";
    let narrower = |set: &ChangeSet| -> BTreeSet<String> {
        set.entities
            .iter()
            .filter(|e| !container(e))
            .map(|e| e.file.clone())
            .collect()
    };
    let (ours_narrower, theirs_narrower) = (narrower(&ours), narrower(&theirs));
    let mine: BTreeSet<&str> = ours.entities.iter().map(|e| e.key.as_str()).collect();
    theirs.both = Some(
        theirs
            .entities
            .iter()
            .filter(|e| {
                mine.contains(e.key.as_str())
                    && !(container(e)
                        && ours_narrower.contains(&e.file)
                        && theirs_narrower.contains(&e.file))
            })
            .map(|e| e.key.clone())
            .collect(),
    );
    Ok(vec![ours, theirs])
}

/// A row for each changed file that is not a node of either snapshot (ctx parses no symbols in it).
fn file_rows(
    dir: &Path,
    (from_tree, to_tree): (&str, &str),
    numstat: &BTreeMap<String, String>,
    entities: &mut Vec<EntityChange>,
) {
    let covered: BTreeSet<String> = entities.iter().map(|e| e.key.clone()).collect();
    for (path, lines) in numstat {
        let key = format!("file:{path}");
        if !covered.contains(&key) {
            let change = if run(dir, &["cat-file", "-e", &format!("{from_tree}:{path}")]).is_err() {
                "added"
            } else if run(dir, &["cat-file", "-e", &format!("{to_tree}:{path}")]).is_err() {
                "removed"
            } else {
                "modified"
            };
            entities.push(EntityChange {
                key,
                kind: "file".into(),
                file: path.clone(),
                change: change.into(),
                lines: lines.clone(),
                commits: Vec::new(),
            });
        }
    }
}

/// The commits of the range that touched each entity, newest first: for an added or modified symbol the commits of
/// its lines at the end of the range (`git log -L`); for a removed or moved symbol the commits that changed how often
/// its text is in its files (`git log -S`, without rename pairs: a move changes no lines); for a file and a node of a
/// whole file, the commits of the file.
fn link_commits(
    dir: &Path,
    range: &str,
    (before, after): (&BTreeMap<String, Node>, &BTreeMap<String, Node>),
    (old, new): (&mut Blobs, &mut Blobs),
    entities: &mut [EntityChange],
) -> Result<()> {
    for entity in entities {
        // shortcut: one git log per changed entity; batch by file when change sets of hundreds of symbols get slow
        let log = match (before.get(&entity.key), after.get(&entity.key)) {
            _ if entity.key.starts_with("file:") => None,
            (Some(a), _) if entity.change == "removed" => {
                let (text, whole) = old.bytes(a)?;
                (!whole).then(|| pickaxe(dir, range, text, &[&a.file]))
            }
            (Some(a), Some(b)) if entity.change == "moved" => {
                let (text, whole) = new.bytes(b)?;
                (!whole).then(|| pickaxe(dir, range, text, &[&a.file, &b.file]))
            }
            (_, Some(b)) => match (new.bytes(b)?.1, b.line, b.end_line) {
                (false, Some(line), Some(end)) => Some(git(
                    dir,
                    &[
                        "log",
                        "--format=%h",
                        "-s",
                        &format!("-L{line},{end}:{}", b.file),
                        range,
                    ],
                )),
                _ => None,
            },
            _ => None,
        };
        let log = match log {
            Some(log) => log?,
            None => git(dir, &["log", "--format=%h", range, "--", &entity.file])?,
        };
        entity.commits = log.lines().map(String::from).collect();
    }
    Ok(())
}

/// `git log -S<text>` of the range in these files, without rename pairs.
fn pickaxe(dir: &Path, range: &str, text: &[u8], files: &[&str]) -> Result<String> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut arg = b"-S".to_vec();
    arg.extend_from_slice(text);
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["log", "--format=%h", "--no-renames"])
        .arg(std::ffi::OsStr::from_bytes(&arg))
        .arg(range)
        .arg("--")
        .args(files)
        .output()
        .context("running git log -S")?;
    ensure!(
        out.status.success(),
        "git log -S: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The stable patch id (`git patch-id --stable`) of each commit of `range` that changes a file: (patch id, commit).
/// A rebase without conflicts keeps a commit's patch id.
///
/// # Errors
/// git fails in `dir`.
pub fn patch_ids(dir: &Path, range: &str) -> Result<Vec<(String, String)>> {
    let log = run(dir, &["log", "-p", "--no-color", "--no-ext-diff", range])?;
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["patch-id", "--stable"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("running git patch-id")?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    let writer = std::thread::spawn(move || stdin.write_all(&log));
    let out = child.wait_with_output()?;
    writer
        .join()
        .map_err(|_| anyhow::anyhow!("git patch-id input"))??;
    ensure!(out.status.success(), "git patch-id failed");
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(p, c)| (p.into(), c.into()))
        .collect())
}

/// The commits of a range, newest first, with their checkpoint event (by patch id) and provenance.
fn commits(dir: &Path, range: &str) -> Result<Vec<CommitRow>> {
    let db = Db::open_default()?;
    let patches: HashMap<String, String> = patch_ids(dir, range)?
        .into_iter()
        .map(|(patch, commit)| (commit, patch))
        .collect();
    let log = git(
        dir,
        &["log", "--format=%H%x1f%h%x1f%ct%x1f%an%x1f%B%x1e", range],
    )?;
    let mut rows = Vec::new();
    for entry in log.split('\x1e').map(str::trim).filter(|e| !e.is_empty()) {
        let fields: Vec<&str> = entry.splitn(5, '\x1f').collect();
        let [full, commit, time, author, message] = fields[..] else {
            bail!("git log gave '{entry}'");
        };
        let message = message.trim();
        let event = match patches.get(full) {
            Some(patch) => db.last_event(&EventFilter {
                kinds: &[COMMIT_EVENT],
                field: Some(("$.patch_id", patch)),
                naming: None,
            })?,
            None => None,
        };
        let provenance = crate::setup::ctx_bin().map_or(Value::Null, |bin| {
            Command::new(bin)
                .current_dir(dir)
                .args(["blame", "commit", commit, "--format", "json", "--quiet"])
                .output()
                .map_or_else(
                    |e| serde_json::json!({"error": e.to_string()}),
                    |out| {
                        serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
                            serde_json::json!({"error": String::from_utf8_lossy(&out.stderr).trim()})
                        })
                    },
                )
        });
        let mut plan_rows: Vec<String> = message
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .filter(|w| {
                w.strip_prefix("todo-")
                    .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            })
            .map(String::from)
            .collect();
        plan_rows.dedup();
        rows.push(CommitRow {
            commit: commit.into(),
            time: time.parse().unwrap_or(0),
            author: author.into(),
            subject: message.lines().next().unwrap_or_default().into(),
            plan_rows,
            checkpoint: event.as_ref().map(|e| e.id),
            session: event.and_then(|e| {
                e.payload["session"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(String::from)
            }),
            provenance,
        });
    }
    Ok(rows)
}

/// An annotation and its state at each ref asked for.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Annotation {
    pub id: i64,
    pub key: String,
    pub kind: String,
    pub value: Value,
    /// The tree and the symbol's content hash when it was written.
    pub tree: String,
    pub content_hash: String,
    pub session: String,
    pub agent: String,
    pub created: i64,
    pub at: Vec<AnnotationAt>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AnnotationAt {
    #[serde(rename = "ref")]
    pub rev: String,
    pub tree: String,
    /// valid (the key with the same content), stale (other content), absent (no such key), no graph (no snapshot).
    pub state: String,
}

/// Write an annotation on the symbol with this stable key, as it is at `rev` (its snapshot is made when missing).
///
/// # Errors
/// The value is not JSON, the key is not at the ref, or the write fails.
pub fn annotate(
    dossiers: &Dossiers,
    dir: &Path,
    (key, kind, value): (&str, &str, &str),
    rev: &str,
    (session, agent): (&str, &str),
) -> Result<i64> {
    let value: Value = serde_json::from_str(value).context("the value must be JSON")?;
    let (tree, db) = snapshot_for(dir, rev, true)?;
    let node = nodes(&db)?
        .remove(key)
        .ok_or_else(|| coded(Code::NotFound, format!("no symbol {key} at {rev}")))?;
    let content_hash = Blobs {
        dir,
        tree: &tree,
        read: HashMap::new(),
    }
    .content_hash(&node)?;
    let writer = dossiers.writer(dir)?;
    writer.conn().execute(
        "INSERT INTO annotations (repo, key, kind, value, tree, content_hash, session, agent, created)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            main_checkout(dir)?.to_string_lossy(),
            key,
            kind,
            value.to_string(),
            tree,
            content_hash,
            session,
            agent,
            now()
        ],
    )?;
    Ok(writer.conn().last_insert_rowid())
}

/// The annotations of the repository (or of one key), each with its state at each ref (default HEAD).
///
/// # Errors
/// A ref does not resolve, or a read fails.
pub fn annotations(
    dossiers: &Dossiers,
    dir: &Path,
    key: Option<&str>,
    revs: &[String],
) -> Result<Vec<Annotation>> {
    let head = ["HEAD".to_string()];
    let revs = if revs.is_empty() { &head[..] } else { revs };
    let graf = graf(dir)?;
    let mut at = Vec::new();
    for rev in revs {
        let (_, tree) = resolve(dir, rev)?;
        let db = file(&graf, &tree);
        let graph = if db.exists() { Some(nodes(&db)?) } else { None };
        at.push((rev, tree, graph));
    }
    let facts = dossiers.facts(dir)?;
    let mut stmt = facts.prepare(
        "SELECT id, key, kind, value, tree, content_hash, session, agent, created FROM annotations
         WHERE repo = ?1 AND (?2 IS NULL OR key = ?2) ORDER BY id",
    )?;
    let rows: Vec<Annotation> = stmt
        .query_map(params![main_checkout(dir)?.to_string_lossy(), key], |r| {
            Ok(Annotation {
                id: r.get(0)?,
                key: r.get(1)?,
                kind: r.get(2)?,
                value: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
                tree: r.get(4)?,
                content_hash: r.get(5)?,
                session: r.get(6)?,
                agent: r.get(7)?,
                created: r.get(8)?,
                at: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for mut row in rows {
        for (rev, tree, graph) in &at {
            let state = match graph.as_ref().map(|g| g.get(&row.key)) {
                None => "no graph",
                Some(None) => "absent",
                Some(Some(node)) => {
                    let mut blobs = Blobs {
                        dir,
                        tree,
                        read: HashMap::new(),
                    };
                    if blobs.content_hash(node)? == row.content_hash {
                        "valid"
                    } else {
                        "stale"
                    }
                }
            };
            row.at.push(AnnotationAt {
                rev: (*rev).clone(),
                tree: tree.clone(),
                state: state.into(),
            });
        }
        out.push(row);
    }
    Ok(out)
}

/// Add (or remove) the lines in the repository's post-commit, post-checkout, post-merge and post-rewrite hooks that
/// run `<legionctl> code graph refresh --background`; other lines of a hook stay.
///
/// # Errors
/// `dir` is not in a git repository, or a hook cannot be written.
pub fn hook(dir: &Path, legionctl: &Path, install: bool) -> Result<Vec<String>> {
    let hooks = PathBuf::from(git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
    )?);
    std::fs::create_dir_all(&hooks)?;
    let line = format!(
        "'{}' code graph refresh --background >/dev/null 2>&1 || true {MARK}",
        legionctl.display()
    );
    let mut done = Vec::new();
    for name in HOOKS {
        let path = hooks.join(name);
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let others: Vec<&str> = text.lines().filter(|l| !l.ends_with(MARK)).collect();
        let body = if install {
            let start = if others.is_empty() {
                vec!["#!/bin/sh"]
            } else {
                others
            };
            format!("{}\n{line}\n", start.join("\n"))
        } else if !path.exists() {
            continue;
        } else if others.iter().all(|l| l.starts_with("#!")) {
            std::fs::remove_file(&path)?;
            done.push(format!("removed {}", path.display()));
            continue;
        } else {
            format!("{}\n", others.join("\n"))
        };
        std::fs::write(&path, body)?;
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
        done.push(format!(
            "{} {}",
            if install { "installed" } else { "cleaned" },
            path.display()
        ));
    }
    if install {
        std::fs::create_dir_all(graf(dir)?.join("refs"))?;
        exclude(dir)?;
    }
    Ok(done)
}
