//! End-to-end: the code graphs (design 23, Part A, P1-P2) through the CLI and the real operator, in a temporary
//! repository with worktrees. ctx is the stand-in of `tests/fixtures/ctx-standin` (CI has no ctx); the ignored test
//! runs the same change scenario with the installed ctx.

mod common;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use common::Operator;
use common::scratch::Scratch;
use serde_json::Value;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// `PATH` with this build's binaries first.
fn path() -> OsString {
    let bin = Path::new(env!("CARGO_BIN_EXE_legionctl")).parent().unwrap();
    std::env::join_paths(
        std::iter::once(bin.to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap()
}

/// `PATH` with `first` before this build's binaries.
fn path_with(first: &Path) -> OsString {
    std::env::join_paths(std::iter::once(first.to_path_buf()).chain(std::env::split_paths(&path())))
        .unwrap()
}

fn standin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ctx-standin")
}

/// An operator with its own HOME (ctx is `ctx` there), and a repository with one commit on main.
struct Lab {
    op: Operator,
    home: Scratch,
    repo: PathBuf,
}

impl Lab {
    fn new(tag: &str, ctx: &Path) -> Lab {
        // outside /tmp and outside any git repository
        let home = Scratch::new(format!(
            "/var/tmp/legionctl-code-{tag}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        // stand-ins a test puts there (tokei, cargo) come before the user's own
        std::fs::create_dir_all(home.join(".cargo/bin")).unwrap();
        std::os::unix::fs::symlink(ctx, home.join(".local/bin/ctx")).unwrap();
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
        let root = home.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let op = Operator::start(
            &format!("code-{tag}"),
            &[
                ("LEGIONCTL_SYSROOT", root.into()),
                ("HOME", home.as_os_str().to_owned()),
                ("PATH", path_with(&home.join(".cargo/bin"))),
            ],
        );
        let repo = home.join("repo");
        let lab = Lab { op, home, repo };
        std::fs::create_dir_all(&lab.repo).unwrap();
        lab.git(&lab.repo, &["init", "--quiet"]);
        write(&lab.repo.join("README.md"), "repo\n");
        write(
            &lab.repo.join("Cargo.toml"),
            "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write(
            &lab.repo.join("src/lib.rs"),
            "mod util;\n\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub fn three() -> i32 {\n    let inc = |x: i32| add(x, 1);\n    \
             inc(2)\n}\n\npub fn two() -> i32 {\n    \
             add(1, 1) + util::helper(0)\n}\n\nfn old_fn() -> i32 {\n    0\n}\n",
        );
        write(
            &lab.repo.join("src/util.rs"),
            "pub fn helper(x: i32) -> i32 {\n    x * 2\n}\n",
        );
        lab.git(&lab.repo, &["add", "."]);
        lab.git(&lab.repo, &["commit", "--quiet", "-m", "start"]);
        lab
    }

    /// git as the user in a terminal: its hooks reach this test's operator, never the user's own.
    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", path())
            .env("XDG_RUNTIME_DIR", &self.op.runtime)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        let home = self.home.to_str().unwrap();
        self.op.run(
            cwd,
            &[("HOME", home), ("LEGIONCTL_SESSION", "s1")],
            args,
            "",
        )
    }

    fn ok(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.run(cwd, args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn json(&self, cwd: &Path, args: &[&str]) -> Value {
        serde_json::from_str(&self.ok(cwd, &[args, &["--json"]].concat())).unwrap()
    }

    fn tree(&self, dir: &Path, rev: &str) -> String {
        self.git(dir, &["rev-parse", &format!("{rev}^{{tree}}")])
    }

    fn graph(&self, tree: &str) -> PathBuf {
        self.repo.join(format!(".graf/refs/{tree}.db"))
    }
}

fn wait_for(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "waited 30 s for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Graph snapshots: none before the repository keeps graphs; then a checkpoint, a plain `git commit` (the hook) and a
/// checkout each give the graph of their tree in the main checkout's `.graf/refs`, shared by the worktrees, with each
/// checkout's `.graf/index.db` on the graph of its HEAD; two refreshes of one tree at once leave one file; a ref
/// without a graph is reported and made on demand; prune keeps heads, bases and merge-bases whatever their age, and
/// the newest 20 others.
#[test]
fn a_graph_is_made_once_per_tree_shared_by_worktrees_and_pruned_to_heads_bases_and_the_newest() {
    let lab = Lab::new("graphs", &standin());
    let repo = lab.repo.clone();
    lab.ok(&repo, &["dossier", "use", "port"]);
    let a = lab.home.join("worktrees/repo/port--a");
    let b = lab.home.join("worktrees/repo/port--b");
    lab.ok(&repo, &["workspace", "open", "--topic", "a"]);
    lab.ok(&repo, &["workspace", "open", "--topic", "b"]);

    // a repository that does not keep graphs gets none from a checkpoint
    write(&a.join("x.txt"), "x");
    lab.ok(&a, &["workspace", "checkpoint", "x"]);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !repo.join(".graf").exists(),
        "no graph before the repository keeps graphs"
    );

    // keep graphs: the first refresh (of main's tree)
    lab.ok(&repo, &["code", "graph", "refresh"]);
    let tree_start = lab.tree(&repo, "HEAD");
    assert!(lab.graph(&tree_start).exists());

    // a checkpoint: the graph of its tree, in the main checkout's .graf; the topic's index.db links to it
    write(&a.join("y.txt"), "y");
    lab.ok(&a, &["workspace", "checkpoint", "y"]);
    let tree_y = lab.tree(&a, "HEAD");
    wait_for("the checkpoint's graph", || lab.graph(&tree_y).exists());
    wait_for("the topic's link", || {
        std::fs::read_link(a.join(".graf/index.db")).is_ok_and(|l| l == lab.graph(&tree_y))
    });
    assert!(
        std::fs::metadata(lab.graph(&tree_y))
            .unwrap()
            .permissions()
            .readonly(),
        "a graph is immutable"
    );
    assert_eq!(
        lab.git(&a, &["status", "--porcelain"]),
        "",
        ".graf is ignored in every worktree"
    );

    // the hook lines (not ctx's own in-place hook); then a commit with git itself in the other worktree
    lab.ok(&repo, &["code", "graph", "hook", "install"]);
    let hook = std::fs::read_to_string(repo.join(".git/hooks/post-commit")).unwrap();
    assert!(hook.contains("code graph refresh --background"), "{hook}");
    write(&b.join("z.txt"), "z");
    lab.git(&b, &["add", "z.txt"]);
    lab.git(&b, &["commit", "--quiet", "-m", "z"]);
    let tree_z = lab.tree(&b, "HEAD");
    wait_for("the hook's graph", || lab.graph(&tree_z).exists());
    wait_for("the other topic's link", || {
        std::fs::read_link(b.join(".graf/index.db")).is_ok_and(|l| l == lab.graph(&tree_z))
    });

    // two refreshes of one new tree at once: one file, no left-overs
    write(&repo.join("both.txt"), "b");
    lab.git(&repo, &["add", "both.txt"]);
    let tree_both = lab.git(&repo, &["write-tree"]);
    lab.git(&repo, &["reset", "--quiet"]);
    std::fs::remove_file(repo.join("both.txt")).unwrap();
    let both = lab.git(&repo, &["commit-tree", &tree_both, "-m", "both"]);
    std::thread::scope(|s| {
        for _ in 0..2 {
            s.spawn(|| lab.ok(&repo, &["code", "graph", "refresh", "--ref", &both]));
        }
    });
    let names = |dir: &Path| -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    };
    let both_files: Vec<String> = names(&repo.join(".graf/refs"))
        .into_iter()
        .filter(|n| n.starts_with(&tree_both))
        .collect();
    assert_eq!(both_files, [format!("{tree_both}.db")]);
    assert!(
        names(&repo.join(".graf"))
            .iter()
            .all(|n| n == "refs" || n == "index.db"),
        "{:?}",
        names(&repo.join(".graf"))
    );

    a_ref_without_a_graph_is_reported_and_made_on_demand(&lab, &tree_y);
    prune_keeps_merge_bases_whatever_their_age_and_the_newest_20(
        &lab,
        &a,
        [&tree_start, &tree_y, &tree_z],
    );

    // a checkout of an old ref: its graph is there, and the checkout's link follows
    lab.git(
        &b,
        &["checkout", "--quiet", "--detach", "legionctl/port--a~5"],
    );
    let tree_old = lab.tree(&b, "HEAD");
    wait_for("the checkout's link", || {
        std::fs::read_link(b.join(".graf/index.db")).is_ok_and(|l| l == lab.graph(&tree_old))
    });
}

/// A ref without a graph: `refs` reports it, a read refuses it, `--build` makes its graph.
fn a_ref_without_a_graph_is_reported_and_made_on_demand(lab: &Lab, tree_y: &str) {
    let repo = &lab.repo;
    // a ref without a graph: reported, refused by a read, made on demand
    let orphan = lab.git(repo, &["commit-tree", tree_y, "-m", "same tree as y"]);
    write(&repo.join("new.txt"), "n");
    lab.git(repo, &["add", "new.txt"]);
    let tree_new = lab.git(repo, &["write-tree"]);
    lab.git(repo, &["reset", "--quiet"]);
    std::fs::remove_file(repo.join("new.txt")).unwrap();
    let nograph = lab.git(
        repo,
        &["commit-tree", &tree_new, "-p", &orphan, "-m", "no graph"],
    );
    lab.git(repo, &["branch", "nograph", &nograph]);
    let refs = lab.json(repo, &["code", "graph", "refs"]);
    let graph_of = |name: &str| {
        refs["refs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == name)
            .unwrap()["graph"]
            .clone()
    };
    assert_eq!(
        (graph_of("legionctl/port--a"), graph_of("nograph")),
        (Value::Bool(true), Value::Bool(false)),
        "{refs}"
    );
    let refused = lab.run(repo, &["code", "graph", "query", "add", "--ref", "nograph"]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("no graph for nograph"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    lab.ok(
        repo,
        &[
            "code", "graph", "query", "add", "--ref", "nograph", "--build",
        ],
    );
    assert!(lab.graph(&tree_new).exists());
}

/// 23 old commits of topic a, each given a graph, after main moved on: prune keeps the newest 20 of them and the
/// topics' merge-base (the start tree, older than the graphs removed).
fn prune_keeps_merge_bases_whatever_their_age_and_the_newest_20(
    lab: &Lab,
    a: &Path,
    [tree_start, tree_y, tree_z]: [&String; 3],
) {
    let repo = &lab.repo;
    // main moves on: the start tree is now only the topics' merge-base
    write(&repo.join("main.txt"), "m");
    lab.git(repo, &["add", "main.txt"]);
    lab.git(
        repo,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "--quiet",
            "-m",
            "main moves",
        ],
    );

    // 23 old commits of topic a (no hook), each given a graph: prune keeps the newest 20 of them, and the heads,
    // bases and merge-bases whatever their age (the start tree is older than the ones removed)
    for i in 0..23 {
        write(&a.join("n.txt"), &i.to_string());
        lab.git(a, &["add", "n.txt"]);
        lab.git(
            a,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--quiet",
                "-m",
                &format!("n{i}"),
            ],
        );
    }
    let old: Vec<String> = (1..=23)
        .rev()
        .map(|k| lab.tree(a, &format!("HEAD~{k}")))
        .collect();
    for k in (1..=23).rev() {
        lab.ok(
            a,
            &["code", "graph", "refresh", "--ref", &format!("HEAD~{k}")],
        );
    }
    let gone: Vec<&String> = old.iter().filter(|t| !lab.graph(t).exists()).collect();
    assert_eq!(
        gone,
        [tree_y, &old[1], &old[2]],
        "the 3 oldest others go (HEAD~23 is the y tree)"
    );
    assert!(old[3..].iter().all(|t| lab.graph(t).exists()));
    for kept in [tree_start, tree_z] {
        assert!(
            lab.graph(kept).exists(),
            "{kept} is a base, merge-base or head"
        );
    }
    // from any folder, with the repository named
    let refs: Value = serde_json::from_str(&lab.op.ok(&[
        "code",
        "graph",
        "refs",
        "--repo",
        repo.to_str().unwrap(),
        "--json",
    ]))
    .unwrap();
    let start_row = refs["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["tree"] == tree_start.as_str())
        .unwrap();
    assert!(
        start_row["refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "merge-base of legionctl/port--a"),
        "{start_row}"
    );
}

/// The change scenario of both change tests: from `a` (the workspace head after open), commit `m` adds `zero` above
/// `add` (its offsets move, its content not) and changes the body of `two`; commit `b` moves `util.rs` to
/// `util/mod.rs` (same module), removes `old_fn` and changes README.md. Both are checkpoints of session s1.
fn scenario(lab: &Lab) -> (PathBuf, [String; 3]) {
    let repo = lab.repo.clone();
    lab.ok(&repo, &["dossier", "use", "port"]);
    lab.ok(&repo, &["workspace", "open"]);
    let ws = lab.home.join("worktrees/repo/port");
    let a = lab.git(&ws, &["rev-parse", "--short", "HEAD"]);
    let lib = std::fs::read_to_string(ws.join("src/lib.rs")).unwrap();
    let lib = lib
        .replace(
            "pub fn add",
            "pub fn zero() -> i32 {\n    0\n}\n\npub fn add",
        )
        .replace("add(1, 1)", "add(2, 0)");
    write(&ws.join("src/lib.rs"), &lib);
    lab.ok(
        &ws,
        &["workspace", "checkpoint", "todo-90: zero, and two adds 2"],
    );
    let m = lab.git(&ws, &["rev-parse", "--short", "HEAD"]);
    std::fs::create_dir_all(ws.join("src/util")).unwrap();
    std::fs::rename(ws.join("src/util.rs"), ws.join("src/util/mod.rs")).unwrap();
    let lib = lib.replace("\nfn old_fn() -> i32 {\n    0\n}\n", "");
    write(&ws.join("src/lib.rs"), &lib);
    write(&ws.join("README.md"), "repo, changed\n");
    lab.ok(
        &ws,
        &["workspace", "checkpoint", "util in a folder, no old_fn"],
    );
    let b = lab.git(&ws, &["rev-parse", "--short", "HEAD"]);
    (ws, [a, m, b])
}

/// (change, key, commits) of a change set's entities.
fn rows(set: &Value) -> BTreeSet<(String, String, String)> {
    set["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let commits: Vec<&str> = e["commits"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c.as_str().unwrap())
                .collect();
            (
                e["change"].as_str().unwrap().into(),
                e["key"].as_str().unwrap().into(),
                commits.join(" "),
            )
        })
        .collect()
}

/// The entity rows that the scenario's a..b must give, with the stand-in and with the real ctx.
fn expected(m: &str, b: &str) -> BTreeSet<(String, String, String)> {
    let both = format!("{b} {m}");
    [
        ("added", "rust:.:zero", m),
        ("modified", "rust:.:two", m),
        ("removed", "rust:.:old_fn", b),
        ("moved", "rust:.:util::helper", b),
        ("modified", "rust:module:.:", both.as_str()),
        ("moved", "rust:module:.:util", b),
        ("modified", "file:README.md", b),
        ("modified", "file:src/lib.rs", both.as_str()),
        ("removed", "file:src/util.rs", b),
        ("added", "file:src/util/mod.rs", b),
    ]
    .into_iter()
    .map(|(c, k, s)| (c.into(), k.into(), s.into()))
    .collect()
}

/// A rebase (todo-119): `--three-way <base> <ours> <theirs>` gives base to ours and base to theirs, so theirs does
/// not show the new work of ours as removed. Both sides change `two`, ours also `three`, theirs also `old_fn`, all in
/// src/lib.rs: only `two` is changed on both sides (the file and its module are no candidates when a function of the
/// file says what changed), with the commit of each side.
#[test]
fn a_three_way_compare_lists_only_the_entities_that_both_sides_changed() {
    let lab = Lab::new("three", &standin());
    lab.ok(&lab.repo, &["dossier", "use", "port"]);
    lab.ok(&lab.repo, &["workspace", "open"]);
    let ws = lab.home.join("worktrees/repo/port");
    let base = lab.git(&ws, &["rev-parse", "--short", "HEAD"]);
    let side = |from: &str, to: &[(&str, &str)], name: &str| {
        let mut lib = std::fs::read_to_string(ws.join("src/lib.rs")).unwrap();
        for (a, b) in to {
            lib = lib.replace(a, b);
        }
        write(&ws.join("src/lib.rs"), &lib);
        lab.git(&ws, &["commit", "--quiet", "-am", name]);
        let head = lab.git(&ws, &["rev-parse", "--short", "HEAD"]);
        lab.git(&ws, &["checkout", "--quiet", "--detach", from]);
        head
    };
    let ours = side(
        &base,
        &[("add(1, 1)", "add(2, 0)"), ("inc(2)", "inc(3)")],
        "ours",
    );
    let theirs = side(
        &base,
        &[
            ("add(1, 1)", "add(1, 2)"),
            ("fn old_fn() -> i32 {\n    0", "fn old_fn() -> i32 {\n    1"),
        ],
        "theirs",
    );

    let refused = lab.run(
        &ws,
        &[
            "code",
            "graph",
            "changes",
            "--three-way",
            &base,
            &ours,
            "--build",
        ],
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("--three-way takes three refs"),
        "{refused:?}"
    );
    let args = [
        "code",
        "graph",
        "changes",
        "--three-way",
        &base,
        &ours,
        &theirs,
        "--build",
    ];
    let sets = lab.json(&ws, &args);
    let functions = |set: &Value| -> Vec<(String, String, String)> {
        rows(set)
            .into_iter()
            .filter(|r| r.1.starts_with("rust:.:"))
            .collect()
    };
    let row = |c: &str, k: &str, s: &str| (c.to_string(), k.to_string(), s.to_string());
    assert_eq!(
        (functions(&sets[0]), functions(&sets[1])),
        (
            vec![
                row("modified", "rust:.:three", &ours),
                row("modified", "rust:.:two", &ours)
            ],
            vec![
                row("modified", "rust:.:old_fn", &theirs),
                row("modified", "rust:.:two", &theirs)
            ],
        ),
        "theirs is from the base, not from ours: {sets:#}"
    );
    assert_eq!(
        [&sets[0]["both"], &sets[1]["both"]],
        [&Value::Null, &serde_json::json!(["rust:.:two"])]
    );
    let text = lab.ok(&ws, &args[..args.len() - 1]);
    let section: Vec<&str> = text
        .lines()
        .skip_while(|l| !l.starts_with("changed on both sides"))
        .collect();
    assert_eq!(
        section,
        [
            "changed on both sides (conflict candidates): 1".to_string(),
            format!("  rust:.:two  {ours}: modified [{ours}]  {theirs}: modified [{theirs}]"),
        ],
        "{text}"
    );
}

/// The change graph by stable keys: exactly the changed entities with the commits that touched them; `add`, whose
/// byte offsets moved, keeps its key and is not a change; a function in a moved file of the same module is moved; the
/// commits carry their checkpoint event, session and plan rows; refs in a row give consecutive change sets; impact
/// maps a stable key to the node id of that ref.
#[test]
fn changes_between_refs_are_the_changed_entities_by_stable_key_with_their_commits() {
    let lab = Lab::new("changes", &standin());
    let (ws, [a, m, b]) = scenario(&lab);

    let refused = lab.run(&ws, &["code", "graph", "changes", &a, &b]);
    assert!(
        !refused.status.success(),
        "a ref without a graph is refused without --build"
    );

    let sets = lab.json(&ws, &["code", "graph", "changes", &a, &b, "--build"]);
    let set = &sets[0];
    assert_eq!(rows(set), expected(&m, &b), "{set:#}");
    let commits = set["commits"].as_array().unwrap();
    assert_eq!(
        commits
            .iter()
            .map(|c| c["commit"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [b.as_str(), m.as_str()]
    );
    for c in commits {
        assert!(c["checkpoint"].is_i64(), "{c}");
        assert_eq!(c["session"], "s1", "{c}");
        assert_eq!(
            c["provenance"]["commit"], c["commit"],
            "ctx blame asked for this commit: {c}"
        );
    }
    assert_eq!(commits[1]["plan_rows"], serde_json::json!(["todo-90"]));
    each_commit_links_its_own_checkpoint_also_after_a_rebase(&lab, &ws);

    // the same symbol, other byte offsets, one key
    let node = |rev: &str| {
        let found = lab.json(&ws, &["code", "graph", "query", "add", "--ref", rev]);
        let n = found["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["kind"] == "function")
            .unwrap()
            .clone();
        (
            n["key"].as_str().unwrap().to_string(),
            n["id"].as_str().unwrap().to_string(),
        )
    };
    let (key_a, id_a) = node(&a);
    let (key_b, id_b) = node(&b);
    assert_eq!(
        (key_a.as_str(), key_b.as_str()),
        ("rust:.:add", "rust:.:add")
    );
    assert_ne!(id_a, id_b, "ctx's ids hold byte offsets");

    // three refs: two change sets; one ref: from its first parent
    let steps = lab.json(&ws, &["code", "graph", "changes", &a, &m, &b, "--build"]);
    let first = rows(&steps[0]);
    let second = rows(&steps[1]);
    assert!(
        first.iter().any(|r| r.1 == "rust:.:zero") && !first.iter().any(|r| r.1 == "rust:.:old_fn")
    );
    assert!(
        second.iter().any(|r| r.1 == "rust:.:old_fn")
            && !second.iter().any(|r| r.1 == "rust:.:zero")
    );
    let one = lab.json(&ws, &["code", "graph", "changes", &b]);
    assert_eq!(
        one[0]["id"], steps[1]["id"],
        "one ref: the change set from its first parent"
    );

    // impact by stable key at b, where add's id is not the one at a
    let impact = lab.json(&ws, &["code", "graph", "impact", "rust:.:add", "--ref", &b]);
    let keys: Vec<&str> = impact["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["key"].as_str().unwrap())
        .collect();
    assert!(keys.contains(&"rust:.:two"), "{impact}");
}

/// The text of a change set for a person: one line per changed file with counts per kind, the code entities under
/// their file, JSON schema keys folded into a count (a schema of a few hundred keys drowned the code in the first
/// dogfood run) unless `--detail`; `--kind` filters the rows, also in the JSON; the JSON has every row.
#[test]
fn the_change_text_is_one_line_per_file_with_counts_and_schema_keys_folded() {
    let lab = Lab::new("text", &standin());
    let repo = lab.repo.clone();
    lab.ok(&repo, &["dossier", "use", "port"]);
    lab.ok(&repo, &["workspace", "open"]);
    let ws = lab.home.join("worktrees/repo/port");
    write(
        &ws.join("schema.json"),
        "{\"properties\": {\"a\": {\"type\": \"string\"}, \"b\": {\"type\": \"integer\"}}, \"type\": \"object\"}\n",
    );
    let lib = std::fs::read_to_string(ws.join("src/lib.rs"))
        .unwrap()
        .replace(
            "pub fn add",
            "pub fn zero() -> i32 {\n    0\n}\n\npub fn add",
        )
        .replace("\nfn old_fn() -> i32 {\n    0\n}\n", "");
    write(&ws.join("src/lib.rs"), &lib);
    lab.ok(
        &ws,
        &["workspace", "checkpoint", "a schema, zero, no old_fn"],
    );

    let all = lab.json(&ws, &["code", "graph", "changes", "HEAD", "--build"]);
    let kinds = |sets: &Value| -> Vec<String> {
        sets[0]["entities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        kinds(&all).iter().filter(|k| *k == "config_key").count(),
        6,
        "the JSON has every schema key: {all:#}"
    );

    let text = lab.ok(&ws, &["code", "graph", "changes", "HEAD"]);
    let lines: Vec<&str> = text.lines().collect();
    let at = |prefix: &str| {
        let found: Vec<usize> = (0..lines.len())
            .filter(|i| lines[*i].starts_with(prefix))
            .collect();
        assert_eq!(found.len(), 1, "one line {prefix:?}:\n{text}");
        found[0]
    };
    let schema = at("  schema.json ");
    assert!(lines[schema].ends_with("config_key +6"), "{text}");
    assert!(!text.contains("config:key:"), "keys are folded:\n{text}");
    let lib = at("  src/lib.rs ");
    assert!(
        lines[lib].contains("function +1 -1") && lines[lib].contains("module ~1"),
        "{text}"
    );
    for key in ["rust:.:zero", "rust:.:old_fn"] {
        let row = (0..lines.len())
            .find(|i| lines[*i].contains(key))
            .unwrap_or_else(|| panic!("{key} listed:\n{text}"));
        assert!(
            row > lib
                && lines[row].starts_with("    ")
                && (lib + 1..row).all(|i| lines[i].starts_with("    ")),
            "{key} is under its file:\n{text}"
        );
    }

    let detail = lab.ok(&ws, &["code", "graph", "changes", "HEAD", "--detail"]);
    assert_eq!(
        detail.matches("config:key:schema.json#/").count(),
        6,
        "{detail}"
    );

    let functions = lab.json(
        &ws,
        &["code", "graph", "changes", "HEAD", "--kind", "function"],
    );
    assert_eq!(kinds(&functions), ["function", "function"], "{functions:#}");
    let text = lab.ok(
        &ws,
        &["code", "graph", "changes", "HEAD", "--kind", "function"],
    );
    assert!(
        !text.contains("schema.json") && text.contains("rust:.:zero"),
        "{text}"
    );
}

/// Two checkpoints with one message and a plain `git commit` with it: each checkpoint's commit links its own
/// checkpoint event, the plain commit none; a rebase onto a moved base keeps the links (the patch id).
fn each_commit_links_its_own_checkpoint_also_after_a_rebase(lab: &Lab, ws: &Path) {
    for (i, by) in ["checkpoint", "checkpoint", "git"].iter().enumerate() {
        write(&ws.join(format!("wip{i}.txt")), "w");
        if *by == "checkpoint" {
            lab.ok(ws, &["workspace", "checkpoint", "wip"]);
        } else {
            lab.git(ws, &["add", "."]);
            lab.git(ws, &["commit", "--quiet", "-m", "wip"]);
        }
    }
    write(&lab.repo.join("base.txt"), "moved");
    lab.git(&lab.repo, &["add", "base.txt"]);
    lab.git(&lab.repo, &["commit", "--quiet", "-m", "base moves"]);
    lab.git(ws, &["rebase", "--quiet", "main"]);
    let sets = lab.json(ws, &["code", "graph", "changes", "main", "HEAD", "--build"]);
    let links: Vec<(String, Option<i64>)> = sets[0]["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["subject"].as_str().unwrap().to_string(),
                c["checkpoint"].as_i64(),
            )
        })
        .collect();
    // a plain commit, then the checkpoints, newest first
    let [(_, git), (_, second), (_, first), (_, b), (_, m)] = &links[..] else {
        panic!("{links:?}");
    };
    assert_eq!(*git, None, "{links:?}");
    assert!(
        [second, first, b, m].iter().all(|l| l.is_some()) && second > first && first > b && b > m,
        "each checkpoint its own event: {links:?}"
    );
}

/// A burst of background refreshes (a rebase runs the hooks once per commit) of two refs: one build at a time in the
/// repository, each tree built once, a waiting request of the same checkout and ref replaced by the newer one, and no
/// job fails.
#[test]
fn a_burst_of_background_refreshes_builds_one_at_a_time_and_each_tree_once() {
    let home = Scratch::new(format!(
        "/var/tmp/legionctl-code-burstctx-{}",
        std::process::id()
    ));
    let (ctx, log) = (home.join("ctx"), home.join("builds"));
    write(
        &ctx,
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n  *'graph index'*) echo + >> {log}; sleep 1; {standin} \"$@\"; s=$?; echo - >> {log}; exit $s;;\n  *) exec {standin} \"$@\";;\nesac\n",
            log = log.display(),
            standin = standin().display()
        ),
    );
    std::fs::set_permissions(&ctx, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let lab = Lab::new("burst", &ctx);
    let repo = lab.repo.clone();
    let mut commits = Vec::new();
    for name in ["one", "two"] {
        write(&repo.join(format!("{name}.txt")), name);
        lab.git(&repo, &["add", "."]);
        lab.git(&repo, &["commit", "--quiet", "-m", name]);
        commits.push(lab.git(&repo, &["rev-parse", "HEAD"]));
    }
    for _ in 0..4 {
        for c in &commits {
            lab.ok(
                &repo,
                &["code", "graph", "refresh", "--background", "--ref", c],
            );
        }
    }
    let jobs = || -> Vec<Value> {
        lab.json(&repo, &["jobs", "list"])["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|j| j["kind"] == "code.graph.refresh")
            .cloned()
            .collect()
    };
    wait_for("the refresh jobs", || {
        let all = jobs();
        all.len() == 8 && all.iter().all(|j| j["state"] != "running")
    });
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "+\n-\n+\n-\n",
        "two builds, one after the other"
    );
    let all = jobs();
    assert!(all.iter().all(|j| j["state"] == "done"), "{all:#?}");
    let replaced = all
        .iter()
        .filter(|j| j["result"].to_string().contains("replaced by job"))
        .count();
    assert!(replaced >= 4, "{all:#?}");
    for c in &commits {
        assert!(lab.graph(&lab.tree(&repo, c)).exists());
    }
}

/// Annotations follow their symbol: valid where its content is the same (also with other byte offsets), stale after a
/// body change, absent after its removal, no graph at a ref without one; a symbol that is not at the ref refuses.
#[test]
fn an_annotation_is_valid_while_its_symbol_is_the_same_stale_after_a_change_and_absent_after_removal()
 {
    let lab = Lab::new("annotations", &standin());
    let (ws, [a, m, b]) = scenario(&lab);
    for key in ["rust:.:add", "rust:.:two", "rust:.:old_fn"] {
        lab.ok(
            &ws,
            &[
                "code",
                "graph",
                "annotate",
                key,
                r#"{"tests":"e2e"}"#,
                "--kind",
                "review",
                "--ref",
                &a,
            ],
        );
    }
    let refused = lab.run(
        &ws,
        &["code", "graph", "annotate", "rust:.:zero", "1", "--ref", &a],
    );
    assert!(!refused.status.success(), "zero is not at a");
    let orphan = lab.git(&ws, &["commit-tree", &lab.tree(&ws, &m), "-m", "x"]);
    write(&ws.join("other.txt"), "o");
    lab.git(&ws, &["add", "other.txt"]);
    let other = lab.git(&ws, &["write-tree"]);
    lab.git(&ws, &["reset", "--quiet"]);
    let nograph = lab.git(
        &ws,
        &["commit-tree", &other, "-p", &orphan, "-m", "no graph"],
    );
    lab.ok(&ws, &["code", "graph", "refresh", "--ref", &m]);
    lab.ok(&ws, &["code", "graph", "refresh", "--ref", &b]);

    let list = lab.json(&ws, &["code", "graph", "annotations", &a, &m, &b, &nograph]);
    let states = |key: &str| -> Vec<String> {
        let row = list
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == key)
            .unwrap();
        assert_eq!(
            (row["kind"].as_str(), row["session"].as_str()),
            (Some("review"), Some("s1"))
        );
        row["at"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["state"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        states("rust:.:add"),
        ["valid", "valid", "valid", "no graph"]
    );
    assert_eq!(
        states("rust:.:two"),
        ["valid", "stale", "stale", "no graph"]
    );
    assert_eq!(
        states("rust:.:old_fn"),
        ["valid", "valid", "absent", "no graph"]
    );
    let one = lab.json(
        &ws,
        &["code", "graph", "annotations", "--key", "rust:.:two"],
    );
    assert_eq!(one.as_array().unwrap().len(), 1, "{one}");
}

/// The change scenario with the installed ctx (`~/.local/bin/ctx`, 2.2.8): the same entity rows as the stand-in.
/// Run it by hand: `cargo test --release -p legionctl --test code -- --ignored`.
#[test]
#[ignore = "needs the real ctx; CI has the stand-in only"]
fn the_real_ctx_gives_the_change_rows_of_the_stand_in() {
    let real = PathBuf::from(std::env::var_os("HOME").unwrap()).join(".local/bin/ctx");
    assert!(real.exists(), "no ctx at {}", real.display());
    let lab = Lab::new("realctx", &real);
    let (ws, [a, m, b]) = scenario(&lab);
    let sets = lab.json(&ws, &["code", "graph", "changes", &a, &b, "--build"]);
    assert_eq!(rows(&sets[0]), expected(&m, &b), "{:#}", sets[0]);
    let impact = lab.json(&ws, &["code", "graph", "impact", "rust:.:add", "--ref", &b]);
    assert!(impact.to_string().contains("rust:.:two"), "{impact}");
}

/// The plan of the fixture: W1 is done (a symbol, a config key, an operation and a unit), W2 is in progress (a file to
/// edit, a function to add, a function to remove).
const FEATURE: &str = r#"// one feature of the fixture
{
  "id": "fx",
  "title": "Fixture feature",
  "status": "in-progress",
  "plan_rows": ["port#todo-1", "port#todo-9"],
  "objectives": [{ "id": "O1", "text": "Numbers." }],
  "stories": [
    { "id": "S1", "text": "add adds.", "objectives": ["O1"], "tests": ["test:tests/e2e.rs::adds"] },
    { "id": "S2", "kind": "requirement", "text": "triple triples.", "objectives": ["O1"],
      "tests": ["test:tests/e2e.rs::triples"] }
  ],
  "work_items": [
    { "id": "W1", "text": "add, its config and operation", "status": "done", "stories": ["S1"], "units": [
      { "key": "rust:.:add", "change": "edit", "loc": 3 },
      { "key": "config:graph.keep", "change": "edit" },
      { "key": "op:code.plan.check", "change": "edit" },
      { "key": "unit:fx-build", "change": "edit", "loc": 2 }
    ] },
    { "id": "W2", "text": "triple in util", "status": "in-progress", "stories": ["S2"], "depends_on": ["W1"],
      "units": [
      { "key": "file:src/util.rs", "change": "edit", "loc": 10 },
      { "key": "rust:.:util::triple", "change": "add", "loc": 4 },
      { "key": "rust:.:old_fn", "change": "remove" }
    ] }
  ]
}
"#;

/// Two plan files with every kind of problem `code plan check` must refuse.
const BAD: &str = r#"{
  "id": "bad", "title": "Bad", "status": "planned",
  "stories": [{ "id": "T1", "text": "x", "tests": ["test:tests/e2e.rs::adds"] }],
  "work_items": [
    { "id": "B1", "text": "x", "status": "planned", "stories": ["S9"], "units": [
      { "key": "crate:fx", "change": "add" },
      { "key": "rust:.:nothing", "change": "edit" }
    ] },
    { "id": "B1", "text": "y", "status": "planned", "units": [] },
    { "id": "B2", "text": "z", "status": "done", "stories": ["T1"], "units": [
      { "key": "rust:.:util::triple", "change": "add" }
    ] }
  ]
}"#;
const WORSE: &str =
    r#"{ "id": "worse", "title": "W", "status": "planned", "owner": "me", "work_items": [] }"#;

/// A work item of `code plan status --json`.
fn item<'a>(status: &'a Value, id: &str) -> &'a Value {
    status["features"][0]["work_items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] == id)
        .unwrap()
}

type UnitRow = (String, String, Value, Value);

/// A unit row with LOC from ctx's span, from tokei, or none.
fn row(key: &str, state: &str, (loc, by): (Value, Value)) -> UnitRow {
    (key.to_string(), state.to_string(), loc, by)
}

fn span(n: u64) -> (Value, Value) {
    (Value::from(n), Value::from("span"))
}

fn tokei(n: u64) -> (Value, Value) {
    (Value::from(n), Value::from("tokei"))
}

fn none() -> (Value, Value) {
    (Value::Null, Value::Null)
}

/// (key, state, `loc_actual`, `loc_by`) of a work item's units.
fn units(item: &Value) -> Vec<UnitRow> {
    item["units"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| {
            (
                u["key"].as_str().unwrap().into(),
                u["state"].as_str().unwrap().into(),
                u["loc_actual"].clone(),
                u["loc_by"].clone(),
            )
        })
        .collect()
}

/// The dossier check findings about the fixture feature.
fn plan_findings(lab: &Lab, ws: &Path) -> Vec<String> {
    let check = lab.run(ws, &["dossier", "check", "--json"]);
    let found: Value = serde_json::from_slice(&check.stdout).unwrap();
    found["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().to_string())
        .filter(|p| p.contains("feature fx"))
        .collect()
}

/// A repository with the fixture plan committed in the workspace of dossier `port`, and tokei's stand-in: the
/// workspace and the plan's commit.
fn plan_fixture(lab: &Lab) -> (PathBuf, String) {
    let tokei = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tokei-standin");
    std::os::unix::fs::symlink(tokei, lab.home.join(".cargo/bin/tokei")).unwrap();
    let repo = lab.repo.clone();
    lab.ok(&repo, &["dossier", "use", "port"]);
    lab.ok(&repo, &["workspace", "open"]);
    let ws = lab.home.join("worktrees/repo/port");

    // tokei counts 3 code lines of util.rs (no comment, no blank); ctx's span of helper is 4 lines
    write(
        &ws.join("src/util.rs"),
        "// helpers\npub fn helper(x: i32) -> i32 {\n\n    x * 2\n}\n",
    );
    write(
        &ws.join("tests/e2e.rs"),
        "#[test]\nfn adds() {\n    assert_eq!(fx::add(1, 2), 3);\n}\n",
    );
    write(
        &ws.join("docs/api/schema.json"),
        r#"{ "code.plan.check": {} }"#,
    );
    write(
        &ws.join("config/defaults.jsonc"),
        "// defaults\n{ \"graph\": { \"keep\": 20 } }\n",
    );
    write(
        &ws.join("planning/units.jsonc"),
        r#"{ "units": [{ "name": "fx-build", "kind": "package", "files": ["Cargo.toml"] }] }"#,
    );
    write(&ws.join("planning/features/fx.jsonc"), FEATURE);
    // the checkpoint's test gate wants a kill for each new test
    lab.ok(
        &ws,
        &[
            "code",
            "tests",
            "mutant",
            "record",
            "src/lib.rs",
            "4",
            "a - b",
            "adds",
        ],
    );
    lab.ok(&ws, &["workspace", "checkpoint", "plan fx"]);
    let planned = lab.git(&ws, &["rev-parse", "--short", "HEAD"]);

    (ws, planned)
}

/// A commit with bad plan files, then gone from the branch: the check names each problem at that ref.
fn a_bad_plan_fails_the_check_with_each_problem(lab: &Lab, ws: &Path) {
    write(&ws.join("planning/features/bad.jsonc"), BAD);
    write(&ws.join("planning/features/worse.jsonc"), WORSE);
    lab.git(ws, &["add", "planning"]);
    lab.git(ws, &["commit", "--quiet", "-m", "bad plan"]);
    let bad = lab.git(ws, &["rev-parse", "--short", "HEAD"]);
    lab.git(ws, &["reset", "--quiet", "--hard", "HEAD^"]);
    let out = lab.run(ws, &["code", "plan", "check", "--ref", &bad, "--build"]);
    assert!(
        !out.status.success(),
        "a plan with problems fails the check"
    );
    let check = lab.json(ws, &["code", "plan", "check", "--ref", &bad]);
    let problems: Vec<&str> = check["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    let at = "planning/features/bad.jsonc";
    let expected = [
        "planning/features/worse.jsonc: unknown field `owner`".to_string(),
        format!("{at}: id B1 twice"),
        format!("{at}: B1 names S9, which does not exist"),
        format!("{at}: B1: no tests (its stories link none)"),
        format!("{at}: B1: crate:fx has no known prefix"),
        format!("{at}: B1: rust:.:nothing resolves to nothing at {bad}"),
        format!("{at}: B1: no tests (its stories link none)"),
        format!("{at}: B2: rust:.:util::triple resolves to nothing at {bad}"),
    ];
    assert_eq!(problems.len(), expected.len(), "{problems:#?}");
    for e in &expected {
        assert!(
            problems.iter().any(|p| p.starts_with(e.as_str())),
            "{e} in {problems:#?}"
        );
    }
}

/// The status of the plan's commit: units, LOC from tokei and spans, tests.
fn the_status_has_units_loc_and_tests(lab: &Lab, ws: &Path) {
    let status = lab.json(ws, &["code", "plan", "status"]);
    let (w1, w2) = (item(&status, "W1"), item(&status, "W2"));
    assert_eq!(
        units(w1),
        [
            row("rust:.:add", "done", span(3)),
            row("config:graph.keep", "done", none()),
            row("op:code.plan.check", "done", none()),
            row("unit:fx-build", "done", tokei(4)),
        ]
    );
    assert_eq!(
        units(w2),
        [
            row("file:src/util.rs", "done", tokei(3)),
            row("rust:.:util::triple", "missing", none()),
            row("rust:.:old_fn", "still there", span(3)),
        ]
    );
    assert_eq!(
        (&w1["complete"], &w1["loc_actual"], &w1["loc_planned"]),
        (&Value::Bool(true), &Value::from(7), &Value::from(5))
    );
    assert_eq!(
        (&w2["complete"], &w2["loc_actual"], &w2["loc_planned"]),
        (&Value::Bool(false), &Value::from(6), &Value::from(14))
    );
    assert_eq!(
        w2["tests"],
        serde_json::json!([{ "key": "test:tests/e2e.rs::triples", "present": false, "result": null, "kills": 0 }])
    );
    assert_eq!(
        (
            &status["features"][0]["loc_actual"],
            &status["features"][0]["loc_planned"]
        ),
        (&Value::from(13), &Value::from(19))
    );
}

/// The commit that adds triple and its test (with its recorded kill) and removes `old_fn`.
fn triple(lab: &Lab, ws: &Path) {
    let util = std::fs::read_to_string(ws.join("src/util.rs")).unwrap();
    write(
        &ws.join("src/util.rs"),
        &format!("{util}\npub fn triple(x: i32) -> i32 {{\n    x * 3\n}}\n"),
    );
    let test = std::fs::read_to_string(ws.join("tests/e2e.rs")).unwrap();
    write(
        &ws.join("tests/e2e.rs"),
        &format!("{test}\n#[test]\nfn triples() {{\n    assert_eq!(fx::util::triple(2), 6);\n}}\n"),
    );
    let lib = std::fs::read_to_string(ws.join("src/lib.rs")).unwrap();
    write(
        &ws.join("src/lib.rs"),
        &lib.replace("\nfn old_fn() -> i32 {\n    0\n}\n", ""),
    );
    lab.ok(
        ws,
        &[
            "code",
            "tests",
            "mutant",
            "record",
            "src/util.rs",
            "9",
            "x * 2",
            "triples",
        ],
    );
    lab.ok(ws, &["workspace", "checkpoint", "triple"]);
}

/// A recorded run where triples failed keeps W2 open; adds passed and has the kill recorded before its checkpoint; a
/// failed `adds` of another crate's `tests/e2e.rs` is not this plan's test.
fn a_failed_run_keeps_its_item_open(lab: &Lab, ws: &Path) {
    let cargo = lab.home.join(".cargo/bin/cargo");
    let artifact = |src: &str, exe: &str| {
        serde_json::json!({"reason": "compiler-artifact", "target": {"src_path": ws.join(src)},
            "executable": format!("/build/target/release/deps/{exe}")})
        .to_string()
    };
    write(
        &cargo,
        &format!(
            "#!/bin/sh\necho '{}'\necho '{}'\n\
             echo '     Running tests/e2e.rs (target/release/deps/e2e-1)' >&2\necho 'test adds ... ok'\n\
             echo 'test triples ... FAILED'\n\
             echo '     Running tests/e2e.rs (target/release/deps/e2e-2)' >&2\necho 'test adds ... FAILED'\nexit 101\n",
            artifact("tests/e2e.rs", "e2e-1"),
            artifact("other/tests/e2e.rs", "e2e-2"),
        ),
    );
    std::fs::set_permissions(&cargo, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    lab.ok(ws, &["code", "tests", "run"]);
    let status = lab.json(ws, &["code", "plan", "status"]);
    let (w1, w2) = (item(&status, "W1"), item(&status, "W2"));
    assert_eq!(
        (&w1["tests"], &w1["complete"]),
        (
            &serde_json::json!([{ "key": "test:tests/e2e.rs::adds", "present": true, "result": "ok", "kills": 1 }]),
            &Value::Bool(true)
        )
    );
    assert_eq!(
        (&w2["tests"][0]["result"], &w2["complete"]),
        (&Value::from("failed"), &Value::Bool(false)),
        "{w2:#}"
    );
}

/// The plan (`planning/`) against the code graph and tokei: `code plan check` passes a correct plan and names each
/// problem of a bad one (unknown field, unknown key prefix, an edit unit and a done item's unit that resolve to
/// nothing, duplicate id, unknown story, no tests); `code plan status` gives each unit done, missing or still there,
/// LOC from tokei for files and units and from the span for symbols; `dossier check` reports a plan row that says done
/// while its feature has open work items, and a row that does not exist; a commit that adds the planned function and
/// test and removes the planned removal completes the work item, and the changes since the plan's commit list
/// exactly the entities that touch it.
#[test]
fn a_plan_is_checked_against_the_code_graph_and_a_commit_that_adds_its_units_completes_it() {
    let lab = Lab::new("plan", &standin());
    let (ws, planned) = plan_fixture(&lab);
    let refused = lab.run(&ws, &["code", "plan", "check"]);
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("no graph for HEAD"),
        "{refused:?}"
    );
    let check = lab.json(&ws, &["code", "plan", "check", "--build"]);
    assert_eq!(
        (&check["features"], &check["problems"]),
        (&Value::from(1), &serde_json::json!([])),
        "{check:#}"
    );

    a_bad_plan_fails_the_check_with_each_problem(&lab, &ws);
    the_status_has_units_loc_and_tests(&lab, &ws);

    // the dossier's plan says todo-1 is built while W2 is open, and todo-9 does not exist
    let dossier = lab.home.join("agentWorkspaces/port");
    let plan = std::fs::read_to_string(dossier.join("plan.md")).unwrap().replace(
        "## ToDo\n",
        "## ToDo\n| id | item | status |\n| --- | --- | --- |\n| todo-1 | fx | built abc123 |\n",
    );
    write(&dossier.join("plan.md"), &plan);
    assert_eq!(
        plan_findings(&lab, &ws),
        [
            "plan#todo-1 says done, but feature fx is not complete at HEAD: W2 (rust:.:util::triple, rust:.:old_fn, \
             test:tests/e2e.rs::triples)",
            "feature fx names plan#todo-9, which does not exist",
        ]
    );

    // the commit that adds triple and its test and removes old_fn completes W2
    triple(&lab, &ws);
    let status = lab.json(
        &ws,
        &[
            "code",
            "plan",
            "status",
            "--feature",
            "fx",
            "--since",
            &planned,
            "--build",
        ],
    );
    let (w1, w2) = (item(&status, "W1"), item(&status, "W2"));
    assert_eq!(
        units(w2),
        [
            row("file:src/util.rs", "done", tokei(6)),
            row("rust:.:util::triple", "done", span(3)),
            row("rust:.:old_fn", "done", none()),
        ]
    );
    assert_eq!(w2["complete"], Value::Bool(true), "{w2:#}");
    assert_eq!(
        w2["changed"],
        serde_json::json!([
            "added rust:.:util::triple",
            "modified file:src/util.rs",
            "modified rust:module:.:util",
            "removed rust:.:old_fn"
        ])
    );
    assert_eq!(
        w1["changed"],
        serde_json::json!([]),
        "lib.rs changed, add did not"
    );
    assert_eq!(
        plan_findings(&lab, &ws),
        ["feature fx names plan#todo-9, which does not exist"]
    );
    a_failed_run_keeps_its_item_open(&lab, &ws);
    let refused = lab.run(&ws, &["code", "plan", "status", "--feature", "nope"]);
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("no feature nope"),
        "{refused:?}"
    );
}
