//! Project checks (design 23, E2) end to end: the real operator and the CLI on fixture repositories in a test home.
//! `detect` reads the language files, the checkpoint and merge gates run the repository's own commands at the tree
//! they commit or merge, each run keeps its log and profile in the dossier home, and a missing kind is a finding.

mod common;
#[path = "common/desktop.rs"]
mod desktop;
#[path = "common/git.rs"]
mod git;
#[path = "common/trust.rs"]
mod trust;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Output;

use common::Operator;
use common::scratch::Scratch;
use git::git;
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
        Lab::with_env(tag, &[])
    }

    /// A lab whose operator also gets `extra` in its environment (a session bus).
    fn with_env(tag: &str, extra: &[(&str, OsString)]) -> Lab {
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
        let mut env = vec![
            ("LEGIONCTL_SYSROOT", home.join("root").into()),
            ("HOME", OsString::from(&*home)),
            ("PATH", path),
            (
                "LEGIONCTL_PLUGIN",
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../integrations/agent-plugin")
                    .into(),
            ),
        ];
        env.extend(extra.iter().cloned());
        let mut op = Operator::start(tag, &env);
        let path = home.to_path_buf();
        op.held.push(home);
        Lab { op, home: path }
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        git(&self.home, dir, args)
    }

    /// A repository `name` with these files in its first commit on main; the user allowed its `.legionctl` files.
    fn repo(&self, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let repo = self.home.join(name);
        for (file, content) in files {
            write(&repo.join(file), content);
            if file.starts_with(".legionctl/") {
                trust::allow(&self.home, &repo.join(file));
            }
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

    /// `legionctl` in `cwd` as the user in a terminal: no agent session.
    fn user(&self, cwd: &Path, args: &[&str]) -> Output {
        self.op
            .run(cwd, &[("HOME", self.home.to_str().unwrap())], args, "")
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
            .join("../../tests/fixtures/claude-code-hooks/session-start.json");
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

const LANGUAGE_CASES: [LanguageCase; 4] = [
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
    // F7 (feedbacks/0001 item 7): a Bazel repository (MODULE.bazel in its top folder)
    (
        "bazel",
        &[("MODULE.bazel", "module(name = \"fx\")\n")],
        "toolchain bazel, lsp none, formatter buildifier, linter buildifier",
        &[
            ("build", "build", "bazel build //..."),
            ("format", "format", "buildifier -mode=check -r ."),
            ("lint", "lint", "buildifier -lint=warn -mode=check -r ."),
            ("test", "test", "bazel test //..."),
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
        // from anywhere, with the repository as the call's folder
        let proposed = lab
            .op
            .ok(&["-C", repo.to_str().unwrap(), "code", "checks", "detect"]);
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

    // detect wrote nothing; its proposal as the repository's file is what the repository checks with, also with the
    // schema link that a repository that checks its JSON files adds (feedbacks/0007)
    let rust = lab.home.join("rust");
    assert!(!rust.join(".legionctl").exists());
    let linked = rust_text.replacen(
        '{',
        "{\n  \"$schema\": \"./.schema/legionctl-checks.json\",",
        1,
    );
    assert_ne!(linked, rust_text);
    write(&rust.join(".legionctl/checks.jsonc"), &linked);
    trust::allow(&lab.home, &rust.join(".legionctl/checks.jsonc"));
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

/// F6 (feedbacks/0001 item 6): JSONC allows trailing commas, and formatters (oxfmt) write them. A repository's
/// `.legionctl/checks.jsonc` and a dossier's `dossier.jsonc` with trailing commas (and `, }` in a string) work end to
/// end: the check runs, the dossier gets its id written (at a registry read) and keeps its title. A layer file with an unfinished comment
/// fails the call with one line that names the file.
#[test]
fn jsonc_files_with_trailing_commas_work_and_an_unfinished_comment_names_its_file() {
    let lab = Lab::new("jsonc");
    let repo = lab.repo(
        "repo",
        &[
            (".gitignore", "/agentWorkspaces\n"),
            (
                ".legionctl/checks.jsonc",
                "// as oxfmt writes it\n{\n  \"checks\": {\n    \"lint\": {\n      \"kind\": \"lint\",\n      \
                 \"run\": \"printf 'a, ]' | grep -q ,\",\n      \"profiles\": [\"report.txt\",],\n    },\n  },\n}\n",
            ),
        ],
    );
    let ws = lab.workspace(&repo, "jsonc");
    let ran = lab.ok(&ws, &["code", "checks", "run", "lint"]);
    assert!(ran.starts_with("lint: exit 0 in "), "{ran}");

    let dossier = lab.home.join("agentWorkspaces/jsonc/dossier.jsonc");
    write(
        &dossier,
        "// written by hand\n{\n  \"title\": \"kept \\\", }\",\n  \"entities\": [],\n}\n",
    );
    // doctor reads the registry from the folders: a folder with no id gets one
    lab.findings(&ws, &["doctor"]);
    let written: Value = serde_json::from_str(
        &std::fs::read_to_string(&dossier)
            .unwrap()
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<String>()
            .replace(",}", "}"),
    )
    .unwrap();
    assert!(
        written["id"].as_str().unwrap().starts_with("dsr-") && written["title"] == "kept \", }",
        "{written}"
    );

    let layer = lab.home.join(".config/legionctl/mine.jsonc");
    write(&layer, "{ \"session\": {} /* not closed\n");
    let out = lab.run(&ws, &["config", "show"]);
    let (_, err) = text(&out);
    assert!(
        !out.status.success()
            && err.lines().count() == 1
            && err.contains(&layer.display().to_string())
            && !err.contains("panicked"),
        "{err}"
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

    // keep_runs 1: the failed run's folder went; an EVIDENCE that names a run keeps it past the limit, also in a
    // subfolder of EVIDENCE (R3-24)
    assert_eq!(
        std::fs::read_dir(&runs).unwrap().count(),
        1,
        "only {passed}"
    );
    write(
        &lab.home
            .join("agentWorkspaces/ckpt/EVIDENCE/checks/01-lint.md"),
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
/// own gate in `.legionctl/` is refused to the agent with the user's exact command. The rewritten gate is not used
/// until the user allows its content (A-5): the checkpoint waits for that answer. The user (a terminal with no agent
/// session, todo-147) runs the merge command: the same merge gate runs, then a desktop dialog asks; Deny keeps main,
/// Allow fast-forwards it. main stays at its commit until the user allows.
#[tokio::test(flavor = "multi_thread")]
async fn a_merge_gate_refuses_an_untracked_file_and_a_branch_that_sets_its_own_gate() {
    let (_bus, address) = private_bus("ships");
    let (_desktop, asked) = desktop::answering(&address, &["allow", "deny", "allow"]).await;
    let lab = Lab::with_env("ships", &[("DBUS_SESSION_BUS_ADDRESS", address.into())]);
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
        r#"{ "checks": { "unit": { "kind": "test", "run": "echo run >> \"$HOME/unit-runs\"", "gate": "merge" } } }"#,
    );
    checkpoint_after_the_users_answer(&lab, &ws, "drop the gate");
    let refused = lab.run(&ws, &["workspace", "merge"]);
    let command = format!(
        "legionctl -C {} workspace merge --dossier ships",
        repo.canonicalize().unwrap().display()
    );
    assert!(
        !refused.status.success()
            && text(&refused).1.contains("an agent cannot merge it")
            && text(&refused)
                .1
                .contains("legionctl workspace merge-request")
            && text(&refused).1.contains(&command),
        "{refused:?}"
    );
    assert_eq!(lab.git(&repo, &["log", "-1", "--format=%s"]), "start");
    assert_eq!(
        asked.lock().unwrap().len(),
        1,
        "the agent's merge asks nobody (one question: the rewritten gate)"
    );

    // the user's terminal, anywhere: the command from the refusal
    let args: Vec<&str> = command.split(' ').skip(1).collect();
    let runs = lab.count("unit-runs");
    let denied = lab.user(&lab.home, &args);
    assert!(
        !denied.status.success() && text(&denied).1.contains("the user did not confirm"),
        "{denied:?}"
    );
    assert_eq!(
        lab.count("unit-runs"),
        runs + 1,
        "the merge gate ran at the rebased head before the dialog"
    );
    assert_eq!(lab.git(&repo, &["log", "-1", "--format=%s"]), "start");
    let allowed = lab.user(&lab.home, &args);
    assert!(allowed.status.success(), "{allowed:?}");
    assert_eq!(
        lab.git(&repo, &["log", "-1", "--format=%s"]),
        "drop the gate"
    );
    let asked = asked.lock().unwrap().clone();
    // the user approves what the diff shows, not only the file name
    assert!(
        asked.len() == 3
            && asked[0].contains(".legionctl/checks.jsonc")
            && asked[1].contains("merge")
            && asked[1]
                .lines()
                .any(|l| l.starts_with('+') && l.contains("echo run")),
        "{asked:?}"
    );
}

/// A private session bus for the stand-in desktop: the bus (it ends when dropped) and its address.
fn private_bus(tag: &str) -> ((common::process::Kill, Scratch), String) {
    let runtime = Scratch::new(format!("/var/tmp/lc-{tag}-bus-{}", std::process::id()));
    let bus_path = runtime.join("bus");
    let bus = common::process::Kill::spawn(
        std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--nopidfile"])
            .arg(format!("--address=unix:path={}", bus_path.display())),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !bus_path.exists() {
        assert!(std::time::Instant::now() < deadline, "the private bus");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let address = format!("unix:path={}", bus_path.display());
    ((bus, runtime), address)
}

/// An agent asks the user to merge a branch that rewrites its own gate (`workspace merge-request`, feedbacks/0001
/// item 1). Only an agent session asks. The gate runs at the rebased head, then the main checkout must be clean and on
/// the base before any dialog: a dirty main checkout is refused with no question. A workspace switched to another
/// branch is refused (its commits are not the dossier's branch). The dialog shows the `.legionctl/` diff; Deny keeps
/// main. A commit that lands on the base while the dialog waits refuses the merge after Allow, and main keeps that
/// commit. Allow lands the commit that the gate tested.
#[tokio::test(flavor = "multi_thread")]
async fn an_agents_merge_request_shows_the_gate_diff_and_lands_only_the_tested_commit() {
    let (_bus, address) = private_bus("mreq");
    let (_desktop, asked) = desktop::answering(&address, &["allow", "deny", "wait", "allow"]).await;
    let lab = Lab::with_env("mreq", &[("DBUS_SESSION_BUS_ADDRESS", address.into())]);
    let repo = lab.repo(
        "repo",
        &[
            (".gitignore", "/agentWorkspaces\n"),
            (
                ".legionctl/checks.jsonc",
                r#"{ "checks": { "unit": { "kind": "test", "run": "echo run >> \"$HOME/unit-runs\"", "gate": "merge" } } }"#,
            ),
        ],
    );
    let ws = lab.workspace(&repo, "mreq");
    // the branch marks its gate binary for git diff, and its command has markup characters for the desktop
    write(&ws.join(".gitattributes"), ".legionctl/* -diff\n");
    write(
        &ws.join(".legionctl/checks.jsonc"),
        r#"{ "checks": { "unit": { "kind": "test", "run": "echo '<pass>' >> \"$HOME/unit-runs\"", "gate": "merge" } } }"#,
    );
    checkpoint_after_the_users_answer(&lab, &ws, "rewrite the gate");
    let questions = || {
        asked
            .lock()
            .unwrap()
            .iter()
            .filter(|b| !b.starts_with("closed "))
            .cloned()
            .collect::<Vec<_>>()
    };
    let main = || lab.git(&repo, &["log", "-1", "--format=%s"]);

    let user = lab.user(&ws, &["workspace", "merge-request"]);
    assert!(
        !user.status.success() && text(&user).1.contains("for agent sessions"),
        "{user:?}"
    );

    write(&repo.join(".gitignore"), "/agentWorkspaces\nedited\n");
    let runs = lab.count("unit-runs");
    let dirty = lab.run(&ws, &["workspace", "merge-request"]);
    assert!(
        !dirty.status.success() && text(&dirty).1.contains("has uncommitted changes"),
        "{dirty:?}"
    );
    assert_eq!(lab.count("unit-runs"), runs + 1, "the gate ran first");
    assert_eq!(questions().len(), 1, "no dialog for a dirty main checkout");
    lab.git(&repo, &["checkout", "--quiet", "--", ".gitignore"]);

    lab.git(&ws, &["switch", "--quiet", "-c", "elsewhere"]);
    write(&ws.join("other.txt"), "not the dossier's branch\n");
    lab.git(&ws, &["add", "other.txt"]);
    lab.git(&ws, &["commit", "--quiet", "-m", "elsewhere"]);
    let switched = lab.run(&ws, &["workspace", "merge-request"]);
    assert!(
        !switched.status.success() && text(&switched).1.contains("is on 'elsewhere'"),
        "{switched:?}"
    );
    assert_eq!(main(), "start");
    lab.git(&ws, &["switch", "--quiet", "legionctl/mreq"]);

    let denied = lab.run(&ws, &["workspace", "merge-request"]);
    assert!(
        !denied.status.success() && text(&denied).1.contains("did not confirm"),
        "{denied:?}"
    );
    assert_eq!(main(), "start");
    let body = questions()[1].clone();
    assert!(
        body.lines()
            .any(|l| l.starts_with('+') && l.contains("echo '&lt;pass>'")),
        "the dialog shows the diff that the user approves, as text and escaped for the desktop's markup: {body}"
    );

    // a commit lands on the base while the dialog waits; the user then allows
    let moved = merge_request_while_a_commit_lands(&lab, &repo, &ws, &questions);
    assert!(
        !moved.status.success() && text(&moved).1.contains("moved"),
        "{moved:?}"
    );
    assert_eq!(main(), "landed during the dialog");

    let allowed = lab.run(&ws, &["workspace", "merge-request"]);
    assert!(allowed.status.success(), "{allowed:?}");
    assert_eq!(
        lab.git(&repo, &["rev-parse", "main"]),
        lab.git(&ws, &["rev-parse", "HEAD"]),
        "main is the tested head, rebased onto the commit that landed"
    );
    assert_eq!(main(), "rewrite the gate");
    assert_eq!(questions().len(), 4);
}

/// A merge request asks nobody before it refuses a branch that leaves `.legionctl/` as it is: the op has no approval of
/// its own (class session), so it must not merge what `workspace merge` (class workspace, it asks) merges.
#[test]
fn a_merge_request_of_a_branch_that_keeps_the_gates_is_refused_and_merges_nothing() {
    let lab = Lab::new("mreq-plain");
    let repo = lab.repo(
        "repo",
        &[
            (".gitignore", "/agentWorkspaces\n"),
            (".legionctl/checks.jsonc", r#"{ "checks": {} }"#),
        ],
    );
    let ws = lab.workspace(&repo, "plain");
    write(&ws.join("feature.txt"), "a feature\n");
    lab.ok(&ws, &["workspace", "checkpoint", "add feature"]);
    let refused = lab.run(&ws, &["workspace", "merge-request"]);
    assert!(
        !refused.status.success()
            && text(&refused)
                .1
                .contains("does not change .legionctl/: merge it with workspace merge"),
        "{refused:?}"
    );
    assert_eq!(lab.git(&repo, &["log", "-1", "--format=%s"]), "start");
}

/// A submodule under `.legionctl/` is part of the gates even when `.gitmodules` tells git to ignore it
/// (`ignore = all`): a branch that moves only its commit is a branch that sets its own gates, so the agent's merge
/// is refused (and a merge request shows the commit in its diff).
#[test]
fn a_gate_submodule_that_git_is_told_to_ignore_still_needs_the_users_merge() {
    let lab = Lab::new("pin");
    let policy = lab.repo("policy", &[("rules.txt", "v1\n")]);
    let repo = lab.repo(
        "repo",
        &[
            (".gitignore", "/agentWorkspaces\n"),
            (".legionctl/checks.jsonc", r#"{ "checks": {} }"#),
        ],
    );
    let add = [
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        "--quiet",
        policy.to_str().unwrap(),
        ".legionctl/policy",
    ];
    lab.git(&repo, &add);
    lab.git(
        &repo,
        &[
            "config",
            "-f",
            ".gitmodules",
            "submodule..legionctl/policy.ignore",
            "all",
        ],
    );
    lab.git(&repo, &["add", ".gitmodules"]);
    lab.git(&repo, &["commit", "--quiet", "-m", "add the policy"]);
    let ws = lab.workspace(&repo, "pin");

    write(&policy.join("rules.txt"), "v2: anything goes\n");
    lab.git(&policy, &["commit", "--quiet", "-am", "loosen"]);
    let loose = lab.git(&policy, &["rev-parse", "HEAD"]);
    let pin = format!("160000,{loose},.legionctl/policy");
    lab.git(&ws, &["update-index", "--cacheinfo", &pin]);
    lab.git(&ws, &["commit", "--quiet", "-m", "move the policy"]);
    let refused = lab.run(&ws, &["workspace", "merge"]);
    assert!(
        !refused.status.success() && text(&refused).1.contains("an agent cannot merge it"),
        "{refused:?}"
    );
    assert_eq!(
        lab.git(&repo, &["log", "-1", "--format=%s"]),
        "add the policy"
    );
}

/// The agent's merge request while a commit lands on the base: the commit lands once the third question waits, then
/// the user allows it in a terminal (`approval answer`); the request's output.
fn merge_request_while_a_commit_lands(
    lab: &Lab,
    repo: &Path,
    ws: &Path,
    questions: &dyn Fn() -> Vec<String>,
) -> Output {
    std::thread::scope(|scope| {
        let waiting = scope.spawn(|| lab.run(ws, &["workspace", "merge-request"]));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while questions().len() < 3 {
            assert!(std::time::Instant::now() < deadline, "the third question");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        write(&repo.join("landed.txt"), "landed during the dialog\n");
        lab.git(repo, &["add", "landed.txt"]);
        lab.git(
            repo,
            &["commit", "--quiet", "-m", "landed during the dialog"],
        );
        // the question that waits: asked and not answered (its event comes after the notice)
        let ask = loop {
            let events: Value =
                serde_json::from_str(&text(&lab.user(&lab.home, &["events", "--json"])).0).unwrap();
            let of = |kind: &str| -> Vec<Value> {
                events["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|e| e["kind"] == kind)
                    .map(|e| e["payload"]["ask"].clone())
                    .collect()
            };
            let answered = of("approval.answer");
            if let Some(ask) = of("approval.ask")
                .into_iter()
                .find(|a| !answered.contains(a))
            {
                break ask.to_string();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the waiting question's event"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let answer = lab.user(&lab.home, &["approval", "answer", &ask, "allow"]);
        assert!(answer.status.success(), "{answer:?}");
        waiting.join().unwrap()
    })
}

/// A checkpoint of `ws` whose changed `.legionctl` waits for the user (A-5): refused with the reason, then, once the
/// user's answer is stored, committed.
fn checkpoint_after_the_users_answer(lab: &Lab, ws: &Path, message: &str) {
    let approvals = || {
        std::fs::read_to_string(lab.home.join(".local/state/legionctl/approvals.json"))
            .unwrap_or_default()
    };
    let before = approvals();
    let waits = lab.run(ws, &["workspace", "checkpoint", message]);
    assert!(
        !waits.status.success()
            && text(&waits)
                .1
                .contains("not used until you allow this file"),
        "{waits:?}"
    );
    for _ in 0..200 {
        if approvals() != before {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    lab.ok(ws, &["workspace", "checkpoint", message]);
}

/// A gate passes only on evidence (R3-3, R3-8, R3-9): a syntax error in the repository's `checks.jsonc` refuses the
/// checkpoint instead of dropping every gate, an install check as a gate is refused and does not run, and a gate check
/// that changes the files while it runs (a fixing formatter) refuses, since its pass is for other content than the
/// commit. Nothing is committed each time.
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
    // the config does not resolve at all: no layer with a syntax error is dropped (todo-142 F-45)
    assert!(
        !refused.status.success() && err.contains(".legionctl/checks.jsonc: expected `,` or `}`"),
        "{err}"
    );
    assert_eq!(commits(), "1");

    // an install check as a gate would install at each checkpoint (R3-8)
    write(
        &ws.join(".legionctl/checks.jsonc"),
        r#"{ "checks": { "install": { "kind": "install", "run": "touch installed", "gate": "checkpoint" } } }"#,
    );
    trust::allow(&lab.home, &ws.join(".legionctl/checks.jsonc"));
    let refused = lab.run(&ws, &["workspace", "checkpoint", "install gate"]);
    assert!(
        !refused.status.success()
            && text(&refused)
                .1
                .contains("check install (install) has a gate")
            && !ws.join("installed").exists(),
        "{refused:?}"
    );

    write(
        &ws.join(".legionctl/checks.jsonc"),
        r#"{ "checks": { "fmt": { "kind": "format", "run": "echo formatted >> code.txt", "gate": "checkpoint" } } }"#,
    );

    trust::allow(&lab.home, &ws.join(".legionctl/checks.jsonc"));
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

/// EV-review-11 U-15: doctor names a repository's missing check kinds once, not again for each topic worktree (19-22
/// equal lines on this machine), and a folder workspace on the home, whose agentWorkspaces is the dossier root
/// itself, is no "legionctl does not replace it" finding.
#[test]
fn doctor_names_a_repositorys_missing_checks_once_and_the_home_is_no_link_finding() {
    let lab = Lab::new("once");
    let repo = lab.repo("repo", &[(".gitignore", "/agentWorkspaces\n")]);
    lab.workspace(&repo, "once");
    for topic in ["t1", "t2"] {
        lab.ok(&repo, &["workspace", "open", "--topic", topic]);
    }
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
    assert_eq!(
        doctor().matches("no checks of kind").count(),
        1,
        "{}",
        doctor()
    );
    lab.ok(&lab.home, &["dossier", "use", "home"]);
    lab.ok(&lab.home, &["workspace", "open"]);
    let found = doctor();
    assert!(!found.contains("legionctl does not replace it"), "{found}");
}

/// A repository without a required kind (`checks.required`) is a finding in `doctor` and `dossier check`, and the
/// agent is told at session start; a check whose command is not found (exit 127) is a "check broken" finding, and the
/// gate names the command and the fix. The checks are the branch's own: once the workspace's file has the kinds, the
/// nudge and the finding go. A worktree that is gone is a finding of its workspace in `doctor`.
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
  "lint": { "kind": "lint", "run": "no-such-linter --check", "gate": "checkpoint" } } }"#,
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
    // the finding is a state for the user (the app's Health); the command is the agent's nudge alone
    assert!(!doctor().contains("code checks detect"), "{}", doctor());

    assert!(
        lab.ok(&ws, &["code", "checks", "run", "lint"])
            .starts_with("lint: exit 127")
    );
    let broken = format!(
        "{}: check lint broken: its command was not found in the operator's PATH (exit 127): no-such-linter --check",
        ws.display()
    );
    assert!(lab.findings(&ws, &["dossier", "check"]).contains(&broken));
    assert!(doctor().contains(&broken));
    // the gate names the command it did not find and the fix (todo-131): from the shell (exit 127) and from a make
    // recipe (make exits 2 and says `Error 127`, as `make lint` without pre-commit on the PATH)
    write(&ws.join("notes.txt"), "notes\n");
    let refusal = |exit: &str| {
        let refused = lab.run(&ws, &["workspace", "checkpoint", "add notes"]);
        let (_, err) = text(&refused);
        assert!(
            !refused.status.success()
                && err.contains(&format!("failed with exit {exit}; log "))
                && err.contains(
                    "\ncommand not found in the operator's PATH (no-such-linter): run legionctl setup apply and log \
                     in again\n"
                ),
            "{err}"
        );
    };
    refusal("127");
    write(&ws.join("Makefile"), "lint:\n\tno-such-linter --check\n");
    write(
        &ws.join(".legionctl/checks.jsonc"),
        r#"{ "checks": { "lint": { "kind": "lint", "run": "make lint", "gate": "checkpoint" } } }"#,
    );
    trust::allow(&lab.home, &ws.join(".legionctl/checks.jsonc"));
    refusal("2");

    write(
        &ws.join(".legionctl/checks.jsonc"),
        r#"{ "checks": {
  "format": { "kind": "format", "run": "true" }, "lint": { "kind": "lint", "run": "true" },
  "build": { "kind": "build", "run": "true" }, "test": { "kind": "test", "run": "true" } } }"#,
    );

    trust::allow(&lab.home, &ws.join(".legionctl/checks.jsonc"));
    assert!(!lab.session_start(&ws).contains("no checks of kind"));
    let checked = lab.findings(&ws, &["dossier", "check"]);
    assert!(
        !checked.contains("no checks of kind") && !checked.contains("broken"),
        "{checked}"
    );

    // a worktree that is gone is a finding of its workspace, not an error of the whole doctor (R3-12)
    std::fs::remove_dir_all(&ws).unwrap();
    let gone = doctor();
    assert!(
        gone.contains(&format!("workspace {}: its folder is gone", ws.display())),
        "{gone}"
    );
}
