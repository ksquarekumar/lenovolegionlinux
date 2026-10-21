//! The settings model (DESIGN-14, DESIGN-20), through the real operator: every config key is a declared setting with
//! user words; a first start writes nothing; a change goes through one checked path (CLI, `legionctl_run`) into the
//! layer it names, with its history and undo; the lists of allowed and forbidden actions bind agents at dispatch and
//! in the git gate; concurrent changes never lose each other; no product code writes a layer file another way.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Output};

mod common;
#[path = "common/desktop.rs"]
mod desktop;
#[path = "common/mcp.rs"]
mod mcp;
#[path = "common/plain.rs"]
mod plain;
#[path = "common/tokens.rs"]
mod tokens;
#[path = "common/trust.rs"]
mod trust;

use common::Operator;
use common::process::Kill;
use common::scratch::Scratch;
use mcp::Mcp;
use serde_json::{Value, json};

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn settings(op: &Operator, args: &[&str]) -> Vec<Value> {
    let view: Value =
        serde_json::from_str(&op.ok(&[&["config", "settings", "--json"][..], args].concat()))
            .unwrap();
    view["items"].as_array().unwrap().clone()
}

fn row(op: &Operator, key: &str) -> Value {
    settings(op, &["--key", key]).remove(0)
}

/// Every setting has a name and help in plain user words, layers that may set it, and only views and operation
/// groups that exist in what it affects; a value the user set shows with its source, the rest come from the defaults.
#[test]
fn every_setting_is_declared_in_user_words_and_shows_where_its_value_comes_from() {
    let home =
        Scratch::new(std::env::temp_dir().join(format!("lc-settings-{}", std::process::id())));
    std::fs::create_dir_all(home.join(".config/legionctl")).unwrap();
    std::fs::write(
        home.join(".config/legionctl/mine.jsonc"),
        r#"{"cache": {"tools": {"max_bytes": 7}}}"#,
    )
    .unwrap();
    let op = Operator::start("settings", &[("HOME", OsString::from(home.as_os_str()))]);
    let items = settings(&op, &[]);
    assert!(items.len() > 50, "{}", items.len());
    let views: Vec<String> = serde_json::from_str::<Value>(&op.ok(&["views", "list", "--json"]))
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap().to_string())
        .collect();
    let groups: Vec<&str> = legionctl_api::OPS
        .iter()
        .map(|o| o.name.split('.').next().unwrap())
        .collect();
    for s in &items {
        let key = s["key"].as_str().unwrap();
        for words in [&s["label"], &s["help"]] {
            let words = words.as_str().unwrap();
            assert!(!words.is_empty(), "{key} has no user words");
            plain::assert_plain(key, words);
        }
        assert!(!s["scope"].as_array().unwrap().is_empty(), "{key}");
        for a in s["affects"].as_array().unwrap() {
            let a = a.as_str().unwrap();
            assert!(
                views.iter().any(|v| v == a) || groups.contains(&a),
                "{key} affects {a}, which is no view and no operation group"
            );
        }
    }
    let cache = items.iter().find(|s| s["key"] == "cache").unwrap();
    assert_eq!(cache["from"], "user", "{cache}");
    assert!(items.iter().any(|s| s["from"] == "defaults"));
}

/// No product code writes a settings layer file but the one writer (`settings_file::edit`, behind `config.set` and
/// the operations that change settings): a direct write would skip the checks, the lock and the history.
#[test]
fn no_code_writes_a_settings_layer_but_the_one_writer() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    // the writer; the files that only read the layers (config) or keep another store in a layer folder (views)
    let allowed = [
        "legionctl-machine/src/settings_file.rs",
        "legionctl-core/src/views.rs",
        "legionctl-core/src/config.rs",
    ];
    let marks = [
        "managed.jsonc",
        "MANAGED",
        "user_dir",
        "Dirs::from_env()",
        "\".legionctl\"",
        ".config/legionctl",
        "/etc/legionctl",
    ];
    let writes = [
        "fs::write(",
        "File::create(",
        "fs::rename(",
        "OpenOptions::new(",
    ];
    let mut found = Vec::new();
    let mut stack = vec![crates.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                if !path.ends_with("tests") && !path.ends_with("target") {
                    stack.push(path);
                }
                continue;
            }
            let name = path.strip_prefix(&crates).unwrap().display().to_string();
            if path.extension().is_none_or(|x| x != "rs")
                || !name.contains("/src/")
                || allowed.contains(&name.as_str())
            {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            // the unit tests of a file are its own fixtures
            let code = source.split("#[cfg(test)]").next().unwrap();
            let lines: Vec<&str> = code.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if !writes.iter().any(|w| line.contains(w)) {
                    continue;
                }
                let near = lines[i.saturating_sub(8)..lines.len().min(i + 3)].join("\n");
                // the image scan's results are files of their own in the project's folder, not settings
                if marks.iter().any(|m| near.contains(m)) && !near.contains("docker-compose-") {
                    found.push(format!("{name}:{}", i + 1));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "settings are written through config.set (legionctl_core::settings::write); these lines write near a \
         settings layer another way: {found:#?}"
    );
}

/// The lab: a home, an operator with a private bus whose stand-in desktop answers the operator's questions in
/// order, and the agent's environment (an unsandboxed harness agent, session s1).
struct Lab {
    op: Operator,
    home: std::path::PathBuf,
    asked: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    _desktop: zbus::Connection,
    _bus: Kill,
    _dir: Scratch,
}

impl Lab {
    async fn new(tag: &str, answers: &'static [&'static str]) -> Lab {
        let dir = Scratch::new(format!("/var/tmp/lc-set-{tag}-{}", std::process::id()));
        let bus_path = dir.join("bus");
        let bus = Kill::spawn(
            Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--nopidfile"])
                .arg(format!("--address=unix:path={}", bus_path.display())),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !bus_path.exists() {
            assert!(std::time::Instant::now() < deadline, "the private bus");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let address = format!("unix:path={}", bus_path.display());
        let (desktop, asked) = desktop::answering(&address, answers).await;
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let op = Operator::start(
            tag,
            &[
                ("HOME", home.clone().into()),
                ("DBUS_SESSION_BUS_ADDRESS", address.into()),
            ],
        );
        Lab {
            op,
            home,
            asked,
            _desktop: desktop,
            _bus: bus,
            _dir: dir,
        }
    }

    fn agent(&self) -> Mcp {
        self.agent_in(&self.home)
    }

    /// The agent, working in `folder`.
    fn agent_in(&self, folder: &Path) -> Mcp {
        Mcp::start(
            &self.op,
            Some(folder),
            &[
                ("HOME", self.home.to_str().unwrap()),
                ("LEGIONCTL_SESSION", "s1"),
            ],
            &[],
        )
    }

    fn fails(&self, args: &[&str]) -> String {
        let out = self.op.cli(args);
        assert!(!out.status.success(), "{args:?} ran: {}", text(&out));
        text(&out)
    }

    fn history(&self) -> Vec<Value> {
        serde_json::from_str::<Value>(&self.op.ok(&["config", "history", "--json"])).unwrap()
            ["items"]
            .as_array()
            .unwrap()
            .clone()
    }
}

/// Each wrong change says what fits and writes nothing: a value of another type or out of its range, a key that is
/// not a setting, a layer outside the setting's scope, a folder that legionctl cannot write, an unset of nothing.
fn wrong_changes_write_nothing(lab: &Lab) {
    for (args, why) in [
        (
            &["config", "set", "dossier.nudge_turns", "many"][..],
            "it takes a whole number from 0",
        ),
        (
            &["config", "set", "views.rlcd_min", "2"],
            "Reuse confidence (0 to 1) (views.rlcd_min): it takes a number from 0 to 1",
        ),
        (
            &["config", "set", "dossier.nudge_turn", "5"],
            "dossier.nudge_turn is not a setting; did you mean dossier.nudge_turns",
        ),
        (
            &[
                "config",
                "set",
                "gateway.port",
                "4800",
                "--scope",
                "project",
            ],
            "Model gateway port (gateway.port) is set only for this computer or you",
        ),
        (
            &["config", "set", "config.overrides", "{}"],
            "is locked: only this computer's administrator can change it",
        ),
        (
            &["config", "set", "dossier.root", "/proc/notes"],
            "Notes folder: legionctl cannot write or make /proc/notes",
        ),
        (
            &["config", "unset", "dossier.gate"],
            "dossier.gate is not set for you",
        ),
    ] {
        let said = lab.fails(args);
        assert!(said.contains(why), "{args:?}: {said}");
    }
}

/// A setting that the user wrote by hand and that does not work is a health finding with its one step.
fn a_broken_value_is_a_health_finding(lab: &Lab) {
    let layer = lab.home.join(".config/legionctl");
    std::fs::write(
        layer.join("mine.jsonc"),
        r#"{"workspace": {"root": "/proc/copies"}}"#,
    )
    .unwrap();
    let root = row(&lab.op, "workspace.root");
    assert_eq!(
        root["value"], "~/.local/share/legionctl/worktrees",
        "the last good value applies"
    );
    let finding = root["finding"].as_str().unwrap().to_string();
    assert_eq!(
        finding,
        "Working copies folder: legionctl cannot write or make /proc/copies. Give your user write access to it, \
         or change the setting. Until then legionctl uses ~/.local/share/legionctl/worktrees."
    );
    let doctor: Value = serde_json::from_slice(&lab.op.cli(&["doctor", "--json"]).stdout).unwrap();
    let health = doctor["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["text"] == finding.as_str())
        .unwrap_or_else(|| panic!("{doctor:#}"));
    assert_eq!(health["actions"][0]["op"], "config.set", "{health}");
    std::fs::remove_file(layer.join("mine.jsonc")).unwrap();
}

/// A first start on defaults writes no settings file; a change is checked (its type and range, its key, the layers
/// it may be set in, its deep check) before one byte is written, lands in your layer with its history row, and
/// Undo of the history puts the value before it back. An agent's change of a safe setting asks once for the toolset
/// that holds the settings operations; a powerful setting asks each time, and a No keeps the value.
#[tokio::test(flavor = "multi_thread")]
async fn a_first_start_writes_nothing_and_each_change_is_checked_kept_and_undone() {
    let lab = Lab::new("change", &["allow", "deny"]).await;
    let layer = lab.home.join(".config/legionctl");
    for args in [
        &["config", "settings"][..],
        &["views", "render", "settings"],
        &["hooks", "list"],
        &["reminders", "list"],
        &["config", "history"],
    ] {
        lab.op.ok(args);
    }
    lab.op.ok(&["dossier", "list"]);
    let notes = lab.home.join("agentWorkspaces");
    for made in [&layer, &notes] {
        assert!(!made.exists(), "a first start made {}", made.display());
    }
    assert_eq!(settings(&lab.op, &["--changed"]), Vec::<Value>::new());

    let said = lab.op.ok(&["config", "set", "dossier.nudge_turns", "7"]);
    assert_eq!(
        said.trim(),
        "Replies before a notes reminder: set for you. In use now: 7."
    );
    plain::assert_plain("config set", &said);
    let turns = row(&lab.op, "dossier.nudge_turns");
    assert_eq!(
        (turns["value"].clone(), turns["from"].clone()),
        (json!(7), json!("user"))
    );

    wrong_changes_write_nothing(&lab);
    assert_eq!(
        lab.history().len(),
        1,
        "a refused change has no history row"
    );

    // the agent: a safe setting asks once for the settings toolset, a powerful one each time
    let mut agent = lab.agent();
    let (out, failed) = agent.run(
        "config.set",
        &json!({"key": "dossier.message_chars", "value": 900}),
    );
    assert!(!failed, "{out}");
    let (out, failed) = agent.run(
        "config.set",
        &json!({"key": "dossier.gate", "value": "false"}),
    );
    assert!(
        failed
            && out["error"]["message"]
                .as_str()
                .unwrap()
                .contains("the user did not allow it"),
        "{out}"
    );
    let asked = lab.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(asked[0].contains("settings-change"), "{asked:?}");
    assert!(asked[1].contains("dossier.gate"), "{asked:?}");
    assert_eq!(
        row(&lab.op, "dossier.gate")["value"],
        true,
        "No keeps the value"
    );
    assert_eq!(row(&lab.op, "dossier.message_chars")["value"], 900);

    a_broken_value_is_a_health_finding(&lab);

    // the history, newest first, with who changed what; Undo of the first change puts the default back
    let history = lab.history();
    let changes: Vec<(&str, &str)> = history
        .iter()
        .map(|c| (c["key"].as_str().unwrap(), c["by"].as_str().unwrap()))
        .collect();
    assert_eq!(
        changes,
        [
            ("dossier.message_chars", "agent:s1"),
            ("dossier.nudge_turns", "user")
        ]
    );
    let first = history[1]["id"].as_i64().unwrap().to_string();
    let undone = lab.op.ok(&["config", "revert", &first]);
    assert!(
        undone.contains("not set for you now. In use now: 5."),
        "{undone}"
    );
    assert_eq!(row(&lab.op, "dossier.nudge_turns")["from"], "defaults");
    assert_eq!(lab.history().len(), 3);
}

/// One list of forbidden and one of allowed actions, each entry with its reason and where it comes from: a
/// forbidden operation is refused to an agent at dispatch and still runs for you; a forbidden git command is refused
/// by the git gate with its reason; removing the entry lets the agent run it again.
#[tokio::test(flavor = "multi_thread")]
async fn the_forbidden_actions_bind_agents_at_dispatch_and_in_the_git_gate() {
    let lab = Lab::new("actions", &[]).await;
    let added = lab.op.ok(&[
        "config",
        "add",
        "actions.forbid",
        "op:dossier.list",
        "--reason",
        "Agents work in their own notes",
    ]);
    assert!(added.contains("Forbidden actions: set for you."), "{added}");
    let added = lab.op.ok(&[
        "config",
        "add",
        "actions.forbid",
        "git:commit --amend",
        "--reason",
        "History stays with you",
    ]);
    plain::assert_plain("config add", &added);
    let said = lab.fails(&[
        "config",
        "add",
        "actions.forbid",
        "dossier.list",
        "--reason",
        "x",
    ]);
    assert!(said.contains("write it as <kind>:<name>"), "{said}");
    let said = lab.fails(&["config", "add", "actions.forbid", "op:boot.*"]);
    assert!(said.contains("op:boot.*: give a reason"), "{said}");

    let entries = row(&lab.op, "actions.forbid")["entries"].clone();
    let from = |pattern: &str| {
        entries
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["pattern"] == pattern)
            .map(|e| e["from"].clone())
    };
    assert_eq!(from("git:push"), Some(json!("defaults")), "{entries:#}");
    assert_eq!(from("op:dossier.list"), Some(json!("user")), "{entries:#}");

    let mut agent = lab.agent();
    let (out, failed) = agent.run("dossier.list", &json!({}));
    assert!(failed, "{out}");
    assert_eq!(
        out["error"]["message"],
        "dossier.list is forbidden for agents: Agents work in their own notes (actions.forbid)"
    );
    lab.op.ok(&["dossier", "list"]);

    // the git gate of a session sandbox: this binary as git, with the real git behind it
    let repo = lab.home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success()
    );
    let gate = lab.home.join("gate");
    std::fs::create_dir_all(&gate).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_legionctl"), gate.join("git")).unwrap();
    let git = |args: &[&str]| {
        Command::new(gate.join("git"))
            .args(args)
            .current_dir(&repo)
            .env_clear()
            .env("XDG_RUNTIME_DIR", &*lab.op.runtime)
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("LEGIONCTL_REAL_GIT", "/usr/bin/git")
            .output()
            .unwrap()
    };
    let amend = git(&["commit", "--amend", "-m", "x"]);
    assert!(
        !amend.status.success()
            && text(&amend).contains(
                "'git commit --amend' is forbidden in this session (actions.forbid): History stays with you"
            ),
        "{}",
        text(&amend)
    );
    let status = git(&["status", "--short"]);
    assert!(status.status.success(), "{}", text(&status));

    a_projects_list_binds_its_agents(&lab, &repo);
    lab.op
        .ok(&["config", "remove", "actions.forbid", "op:dossier.list"]);
    let (out, failed) = agent.run("dossier.list", &json!({}));
    assert!(!failed, "{out}");
    assert!(
        row(&lab.op, "actions.forbid")["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["pattern"] != "op:dossier.list")
    );
}

/// A project's own forbidden entries (once the user allowed its file) bind an agent that works in the project, and
/// only there.
fn a_projects_list_binds_its_agents(lab: &Lab, repo: &Path) {
    let file = repo.join(".legionctl/actions.jsonc");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        r#"{"actions": {"forbid+": [{"pattern": "op:views.list", "reason": "Views stay with the project"}]}}"#,
    )
    .unwrap();
    trust::allow(&lab.home, &file);
    let (out, failed) = lab.agent_in(repo).run("views.list", &json!({}));
    assert!(failed, "{out}");
    assert_eq!(
        out["error"]["message"],
        "views.list is forbidden for agents: Views stay with the project (actions.forbid)"
    );
    let (out, failed) = lab.agent().run("views.list", &json!({}));
    assert!(!failed, "outside the project: {out}");
}

/// Twelve projects get a setting at the same time: each new project file is the user's own, so each is allowed, and
/// the trust answers of all twelve are kept (the answers' file is changed under its lock too).
fn project_answers_at_the_same_time_all_land(op: &std::sync::Arc<Operator>, home: &Path) {
    let repos: Vec<std::path::PathBuf> = (0..12)
        .map(|i| {
            let repo = home.join(format!("p{i}"));
            std::fs::create_dir_all(repo.join(".git")).unwrap();
            repo
        })
        .collect();
    let threads: Vec<_> = repos
        .iter()
        .cloned()
        .map(|repo| {
            let op = op.clone();
            std::thread::spawn(move || {
                let out = op.cli_in(
                    &repo,
                    &[
                        "config",
                        "set",
                        "dossier.gate",
                        "false",
                        "--scope",
                        "project",
                    ],
                );
                assert!(out.status.success(), "{}", text(&out));
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let approvals: Value = serde_json::from_slice(
        &std::fs::read(home.join(".local/state/legionctl/approvals.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(approvals.as_object().unwrap().len(), 12, "{approvals:#}");
    for repo in &repos {
        let gate = op.cli_in(
            repo,
            &["config", "settings", "--key", "dossier.gate", "--json"],
        );
        let row: Value = serde_json::from_slice(&gate.stdout).unwrap();
        assert_eq!(row["items"][0]["from"], "project", "{row}");
    }
}

/// An operation reads the settings of its call's folder, the project's layer too: a project's own package shows in
/// that project only (before, the package operations read the user's settings alone).
#[test]
fn an_operation_reads_the_settings_of_its_calls_project() {
    let home =
        Scratch::new(std::env::temp_dir().join(format!("lc-settings-ctx-{}", std::process::id())));
    let repo = home.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let file = repo.join(".legionctl/packages.jsonc");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        r#"{"packages": {"lint-tool": {"kind": "npm", "source": "lint-tool", "version": "1.0.0"}}}"#,
    )
    .unwrap();
    trust::allow(&home, &file);
    let op = Operator::start("ctx", &[("HOME", OsString::from(home.as_os_str()))]);
    let names = |cwd: &Path| -> Vec<String> {
        let out = op.cli_in(cwd, &["packages", "list", "--json"]);
        assert!(out.status.success(), "{}", text(&out));
        let list: Value = serde_json::from_slice(&out.stdout).unwrap();
        list.to_string()
            .contains("lint-tool")
            .then(|| "lint-tool".to_string())
            .into_iter()
            .collect()
    };
    assert_eq!(names(&repo), ["lint-tool"]);
    assert_eq!(names(&home), Vec::<String>::new());
}

/// A project never widens what an agent may do before the user trusts it: its actions.allow entries (op:, toolset:,
/// tool:) grant nothing until the user's first-use answer of its file, at dispatch and in the grant of a toolset; its
/// actions.forbid entries bind at once (they add to the list and only restrict). After the answer, its allowed
/// operation runs with no toolset question and its toolset may join a grant.
#[tokio::test(flavor = "multi_thread")]
async fn an_untrusted_projects_allowed_actions_grant_nothing_until_the_user_allows_its_file() {
    let lab = Lab::new("untrusted", &["deny", "deny", "deny", "deny"]).await;
    let repo = lab.home.join("repo");
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .status()
            .unwrap()
            .success()
    );
    let sets = lab.home.join(".config/legionctl/capabilities");
    std::fs::create_dir_all(&sets).unwrap();
    std::fs::write(
        sets.join("deploy.jsonc"),
        r#"{ "name": "deploy", "version": 1, "connections": { "out": [{ "host": "deploy.example.com" }] } }"#,
    )
    .unwrap();
    let file = repo.join(".legionctl/actions.jsonc");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        r#"{"actions": {
            "allow+": [{"pattern": "op:facts.set", "reason": "notes"}, {"pattern": "toolset:deploy", "reason": "ship"},
                       {"pattern": "tool:remote/admin", "reason": "admin"}],
            "forbid+": [{"pattern": "op:views.list", "reason": "views stay"}]
        }}"#,
    )
    .unwrap();
    let in_repo = |args: &[&str]| -> Value {
        let out = lab.op.cli_in(&repo, &[args, &["--json"]].concat());
        assert!(out.status.success(), "{args:?}: {}", text(&out));
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let patterns = |key: &str| -> Vec<String> {
        in_repo(&["config", "settings", "--key", key])["items"][0]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["pattern"].as_str().unwrap().to_string())
            .collect()
    };
    let deploy = |check: &Value| {
        check["sets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == "deploy")
            .unwrap()["refusal"]
            .clone()
    };
    // before the answer: none of the project's entries is used, and the file waits
    for pattern in ["op:facts.set", "toolset:deploy", "tool:remote/admin"] {
        assert!(
            !patterns("actions.allow").iter().any(|p| p == pattern),
            "{pattern} used before the answer"
        );
    }
    assert!(patterns("actions.forbid").contains(&"op:views.list".to_string()));
    assert_eq!(
        deploy(&in_repo(&["runnables", "check"])),
        "your settings do not allow it (actions.allow)"
    );
    let mut agent = lab.agent_in(&repo);
    let (out, failed) = agent.run(
        "facts.set",
        &json!({"entity": "x", "attribute": "y", "value": "1"}),
    );
    assert!(
        failed
            && out["error"]["message"]
                .as_str()
                .unwrap()
                .contains("needs the toolset session-own"),
        "the project's allowed operation still asks: {out}"
    );
    let (_, failed) = agent.run("views.list", &json!({}));
    assert!(failed, "the project's forbidden entry binds at once");

    trust::allow(&lab.home, &file);
    let asked = lab.asked.lock().unwrap().len();
    let (out, failed) = agent.run(
        "facts.set",
        &json!({"entity": "x", "attribute": "y", "value": "1"}),
    );
    assert!(!failed, "{out}");
    assert_eq!(
        lab.asked.lock().unwrap().len(),
        asked,
        "no toolset question after the answer"
    );
    assert_eq!(deploy(&in_repo(&["runnables", "check"])), Value::Null);
    assert!(patterns("actions.allow").contains(&"tool:remote/admin".to_string()));
}

/// The forbidden list adds up across the layers: this computer's, yours and a project's entries all bind; a project
/// file that sets the list to [] removes nothing below it; you remove your own entry, and this computer's stays.
#[tokio::test(flavor = "multi_thread")]
async fn the_forbidden_list_adds_up_and_a_layer_removes_only_its_own_entries() {
    let lab = Lab::new("union", &[]).await;
    let repo = lab.home.join("repo");
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .status()
            .unwrap()
            .success()
    );
    std::fs::create_dir_all(repo.join(".legionctl")).unwrap();
    std::fs::write(
        repo.join(".legionctl/a-forbid.jsonc"),
        r#"{"actions": {"forbid": [{"pattern": "op:views.list", "reason": "views stay"}]}}"#,
    )
    .unwrap();
    std::fs::write(
        repo.join(".legionctl/b-empty.jsonc"),
        r#"{"actions": {"forbid": []}}"#,
    )
    .unwrap();
    lab.op.ok(&[
        "config",
        "add",
        "actions.forbid",
        "op:jobs.list",
        "--reason",
        "mine",
    ]);
    let mut agent = lab.agent_in(&repo);
    for (op, binds) in [
        ("views.list", true),
        ("jobs.list", true),
        ("dossier.list", false),
    ] {
        let (out, failed) = agent.run(op, &json!({}));
        assert_eq!(failed, binds, "{op}: {out}");
    }
    let out = lab.op.cli_in(
        &repo,
        &["config", "settings", "--key", "actions.forbid", "--json"],
    );
    let row: Value = serde_json::from_slice(&out.stdout).unwrap();
    let from = |pattern: &str| {
        row["items"][0]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["pattern"] == pattern)
            .map(|e| e["from"].clone())
    };
    assert_eq!(
        (
            from("git:push"),
            from("op:jobs.list"),
            from("op:views.list")
        ),
        (
            Some(json!("defaults")),
            Some(json!("user")),
            Some(json!("project"))
        ),
        "the project's [] removes nothing below it"
    );
    let below = lab.fails(&["config", "remove", "actions.forbid", "git:push"]);
    assert!(
        below.contains("git:push is not one of the entries for you"),
        "{below}"
    );
    lab.op
        .ok(&["config", "remove", "actions.forbid", "op:jobs.list"]);
    let (out, failed) = lab.agent().run("jobs.list", &json!({}));
    assert!(!failed, "{out}");
}

/// Trust answers that arrive at the same moment are all kept (the answers' file is changed under its lock): sixteen
/// projects with a file that waits for an answer are read at once, the desktop allows each, and each answer is stored.
/// Before, two answers read the same file and the second rename dropped the first one.
#[tokio::test(flavor = "multi_thread")]
async fn trust_answers_at_the_same_moment_are_all_kept() {
    let lab = Lab::new("answers", &["allow"; 16]).await;
    let repos: Vec<std::path::PathBuf> = (0..16)
        .map(|i| {
            let repo = lab.home.join(format!("p{i}"));
            std::fs::create_dir_all(repo.join(".git")).unwrap();
            std::fs::create_dir_all(repo.join(".legionctl")).unwrap();
            std::fs::write(
                repo.join(".legionctl/gate.jsonc"),
                format!(r#"{{"dossier": {{"gate": false, "nudge_turns": {i}}}}}"#),
            )
            .unwrap();
            repo
        })
        .collect();
    let op = &lab.op;
    std::thread::scope(|scope| {
        for repo in &repos {
            scope.spawn(move || {
                // the file waits for its answer, so config show reports it (and exits 1)
                let out = op.cli_in(repo, &["config", "show"]);
                assert!(
                    text(&out).contains("is not used until you allow"),
                    "{}",
                    text(&out)
                );
            });
        }
    });
    let approvals = lab.home.join(".local/state/legionctl/approvals.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let asked = lab.asked.lock().unwrap().len();
        let stored = std::fs::read(&approvals)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .map_or(0, |v| v.as_object().map_or(0, serde_json::Map::len));
        if asked == 16 && stored == 16 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{asked} questions, {stored} answers kept"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// A forbidden git command of a capability set binds the sessions that hold the set, in the git gate: a session with
/// the set is refused it, one without the set runs it.
#[test]
fn a_sets_forbidden_git_command_binds_only_the_sessions_that_hold_the_set() {
    let home =
        Scratch::new(std::env::temp_dir().join(format!("lc-settings-set-{}", std::process::id())));
    let op = Operator::start("setgate", &[("HOME", OsString::from(home.as_os_str()))]);
    op.ok(&[
        "config",
        "add",
        "actions.forbid",
        "git:tag",
        "--reason",
        "Releases stay with you",
        "--set",
        "rust-dev",
    ]);
    tokens::add_session_token(&home, "with-token", "with", &["rust-dev"]);
    tokens::add_session_token(&home, "without-token", "without", &[]);
    let repo = home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success()
    );
    let gate = home.join("gate");
    std::fs::create_dir_all(&gate).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_legionctl"), gate.join("git")).unwrap();
    let tag = |token: &str| {
        Command::new(gate.join("git"))
            .args(["tag", "--list"])
            .current_dir(&repo)
            .env_clear()
            .env("XDG_RUNTIME_DIR", &*op.runtime)
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("LEGIONCTL_REAL_GIT", "/usr/bin/git")
            .env("LEGIONCTL_TOKEN", token)
            .output()
            .unwrap()
    };
    let with = tag("with-token");
    assert!(
        !with.status.success()
            && text(&with).contains(
                "'git tag' is forbidden in this session (actions.forbid): Releases stay with you"
            ),
        "{}",
        text(&with)
    );
    let without = tag("without-token");
    assert!(without.status.success(), "{}", text(&without));
}

/// The default folder of the working copies is under the home now; an upgrade keeps the old default for a user whose
/// workspaces are there, through the one writer and its history, so nothing moves; a user with no workspace there, or
/// with a folder of their own, keeps what they have.
#[test]
fn an_upgrade_keeps_the_old_working_copies_folder_where_the_workspaces_are() {
    let start = |home: &Path, tag: &str| {
        Operator::start(tag, &[("HOME", OsString::from(home.as_os_str()))])
    };
    let root = |op: &Operator| row(op, "workspace.root");
    let open_workspace = |home: &Path| {
        let added = Command::new("python3")
            .args([
                "-c",
                "import sqlite3, sys; c = sqlite3.connect(sys.argv[1]); \
                 c.execute(\"INSERT INTO workspaces (dossier, repo, path, branch, base, opened) VALUES \
                 ('d', '/r', '/data/worktrees/r/d', 'legionctl/d', 'main', 1)\"); c.commit()",
            ])
            .arg(home.join(".local/state/legionctl/operator.db"))
            .status()
            .unwrap();
        assert!(added.success());
    };

    // a new user: the folder under the home, made absolute
    let fresh =
        Scratch::new(std::env::temp_dir().join(format!("lc-up-new-{}", std::process::id())));
    let op = start(&fresh, "up-new");
    let new = root(&op);
    assert_eq!(
        (new["value"].clone(), new["from"].clone()),
        (
            json!("~/.local/share/legionctl/worktrees"),
            json!("defaults")
        )
    );
    let explained = op.ok(&["config", "settings", "--key", "workspace.root"]);
    assert!(
        explained.contains("~/.local/share/legionctl/worktrees"),
        "{explained}"
    );
    drop(op);

    // a user whose workspaces are at the old default: kept for them, with its history row
    let old = Scratch::new(std::env::temp_dir().join(format!("lc-up-old-{}", std::process::id())));
    start(&old, "up-old1").ok(&["config", "history"]);
    open_workspace(&old);
    let op = start(&old, "up-old2");
    let kept = root(&op);
    assert_eq!(
        (kept["value"].clone(), kept["from"].clone()),
        (json!("/data/worktrees"), json!("user"))
    );
    let history: Value = serde_json::from_str(&op.ok(&["config", "history", "--json"])).unwrap();
    assert_eq!(history["items"].as_array().unwrap().len(), 1, "{history:#}");
    assert_eq!(history["items"][0]["by"], "upgrade");
    drop(op);
    start(&old, "up-old3").ok(&["config", "history"]);
    let history: Value =
        serde_json::from_str(&start(&old, "up-old4").ok(&["config", "history", "--json"])).unwrap();
    assert_eq!(
        history["items"].as_array().unwrap().len(),
        1,
        "once: {history:#}"
    );

    // a user with a folder of their own keeps it
    let own = Scratch::new(std::env::temp_dir().join(format!("lc-up-own-{}", std::process::id())));
    std::fs::create_dir_all(own.join(".config/legionctl")).unwrap();
    std::fs::write(
        own.join(".config/legionctl/mine.jsonc"),
        format!(
            r#"{{"workspace": {{"root": "{}"}}}}"#,
            own.join("copies").display()
        ),
    )
    .unwrap();
    start(&own, "up-own1").ok(&["config", "history"]);
    open_workspace(&own);
    let op = start(&own, "up-own2");
    assert_eq!(root(&op)["value"], own.join("copies").display().to_string());
    assert_eq!(settings(&op, &["--changed"]).len(), 1, "nothing written");
}

/// Changes that run at the same time never lose each other (review/16 B10): each takes the layer's lock, writes its
/// own temporary file and renames it. Before, two writers read the same file and the second rename dropped the first
/// one's key. The operations that change settings for you (reminders, hooks) go the same way, with their history;
/// removing a hook action drops its bindings.
#[test]
fn changes_at_the_same_time_all_land_with_their_history() {
    let home =
        Scratch::new(std::env::temp_dir().join(format!("lc-settings-race-{}", std::process::id())));
    let op = Operator::start("race", &[("HOME", OsString::from(home.as_os_str()))]);
    let op = std::sync::Arc::new(op);
    let threads: Vec<_> = (0..24)
        .map(|i| {
            let op = op.clone();
            std::thread::spawn(move || {
                let id = format!("note-{i}");
                let out = op.cli(&["reminders", "add", &id, &format!("Note {i}.")]);
                assert!(out.status.success(), "{id}: {}", text(&out));
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let reminders: Value = serde_json::from_str(&op.ok(&["reminders", "list", "--json"])).unwrap();
    let ids: Vec<&str> = reminders
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["id"].as_str().filter(|id| id.starts_with("note-")))
        .collect();
    assert_eq!(ids.len(), 24, "lost changes: {ids:?}");
    let history: Value = serde_json::from_str(&op.ok(&[
        "config",
        "history",
        "--key",
        "reminders",
        "--limit",
        "100",
        "--json",
    ]))
    .unwrap();
    assert_eq!(
        history["items"].as_array().unwrap().len(),
        24,
        "{history:#}"
    );

    project_answers_at_the_same_time_all_land(&op, &home);
    op.ok(&["hooks", "add", "greet", "echo hello"]);
    op.ok(&["hooks", "bind", "prompt", "greet"]);
    op.ok(&["hooks", "rm", "greet"]);
    let hooks: Value = serde_json::from_str(&op.ok(&["hooks", "list", "--json"])).unwrap();
    assert!(
        !hooks.to_string().contains("greet"),
        "removing an action drops its bindings: {hooks:#}"
    );
}
