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

fn standin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ctx-standin")
}

/// An operator with its own HOME (ctx is `ctx` there), and a repository with one commit on main.
struct Lab {
    op: Operator,
    home: PathBuf,
    repo: PathBuf,
}

impl Lab {
    fn new(tag: &str, ctx: &Path) -> Lab {
        // outside /tmp and outside any git repository
        let home =
            Path::new("/var/tmp").join(format!("legionctl-code-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
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
                ("HOME", home.clone().into()),
                ("PATH", path()),
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

impl Drop for Lab {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
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
