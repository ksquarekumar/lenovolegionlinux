//! End-to-end: agent archetypes, configs and teams (design 23, B+2 to B+4, P6a) through the CLI and the real
//! operator: `agents check` resolves the layers (archetype file, `agents.configs.<name>`, team) and refuses each field
//! that a member's run path cannot apply and each layer that widens; `setup apply` writes each config as a Codex agent
//! and the systemd user manager's PATH (environment.d).

mod common;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::Operator;
use common::scratch::Scratch;
use serde_json::Value;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn shipped_plugin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../integrations/agent-plugin")
}

/// An operator with its own HOME, a git repository, and the agent plugin at `plugin`.
struct Lab {
    op: Operator,
    home: Scratch,
    repo: PathBuf,
}

impl Lab {
    fn new(tag: &str, plugin: &Path) -> Lab {
        let home = Scratch::new(format!(
            "/var/tmp/legionctl-agents-{tag}-{}",
            std::process::id()
        ));
        let repo = home.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        std::fs::create_dir_all(home.join("root")).unwrap();
        let op = Operator::start(
            &format!("ag-{tag}"),
            &[
                ("LEGIONCTL_SYSROOT", home.join("root").into()),
                ("HOME", home.as_os_str().to_owned()),
                ("PATH", std::env::var_os("PATH").unwrap()),
                ("LEGIONCTL_PLUGIN", OsString::from(plugin)),
            ],
        );
        Lab { op, home, repo }
    }

    /// `setup apply` also installs ctx's skill and server: a stand-in ctx that does nothing.
    fn stand_in_ctx(&self) {
        let ctx = self.home.join(".local/bin/ctx");
        write(&ctx, "#!/bin/sh\necho ctx 0.0.0\n");
        std::fs::set_permissions(&ctx, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
    }

    /// The Codex agent items of `setup check`: (what, status).
    fn setup_agent_items(&self) -> Vec<(String, String)> {
        let check: Value =
            serde_json::from_str(&self.op.ok(&["setup", "check", "--json"])).unwrap();
        check["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["what"].as_str().unwrap().starts_with("legionctl agent"))
            .map(|i| {
                (
                    i["what"].as_str().unwrap().into(),
                    i["status"].as_str().unwrap().into(),
                )
            })
            .collect()
    }

    fn user_config(&self, text: &str) {
        write(&self.home.join(".config/legionctl/agents.jsonc"), text);
    }

    /// The JSON of `agents check <args>` in the repository, and whether the text form exits 0.
    fn check(&self, args: &[&str]) -> (Value, bool) {
        let mut command = vec!["agents", "check"];
        command.extend(args);
        let text = self.op.cli_in(&self.repo, &command);
        command.push("--json");
        let out = self.op.cli_in(&self.repo, &command);
        assert!(
            out.status.success(),
            "{command:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        (
            serde_json::from_slice(&out.stdout).unwrap(),
            text.status.success(),
        )
    }
}

fn file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn member<'a>(check: &'a Value, name: &str) -> &'a Value {
    check["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == name)
        .unwrap_or_else(|| panic!("no member {name} in {check:#}"))
}

/// The refusals, each cut at its first " (": `<member>: <field>` for a field that a run path cannot apply.
fn refused(check: &Value) -> Vec<String> {
    check["refused"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn vanilla_resolves_its_three_members_through_their_layers_and_a_layer_that_widens_is_refused() {
    let lab = Lab::new("layers", &shipped_plugin());
    lab.user_config(
        r#"{ "agents": { "configs": {
            "main-agent": { "tools": { "allow": ["Read", "Grep", "Bash", "Edit"] },
                            "commands": { "deny": ["Bash(cargo publish*)"] } },
            "thread-observer": { "model": "sonnet" }
        } } }"#,
    );
    let (vanilla, ok) = lab.check(&["--team", "vanilla", "--harness", "claude"]);
    assert!(ok && refused(&vanilla).is_empty(), "{vanilla:#}");
    assert_eq!(vanilla["members"].as_array().unwrap().len(), 3);

    let main = member(&vanilla, "main0");
    assert_eq!(main["run_path"], "claude-session");
    assert_eq!(main["fields"]["tools"]["allow"][3], "Edit");
    assert_eq!(main["set_by"]["tools"], "agents.configs.main-agent");
    assert_eq!(main["fields"]["may"], serde_json::json!(["note"]));

    // each layer in its place: the archetype file's effort, the config's model and day budget, the team's wake budget
    let observer = member(&vanilla, "observer0");
    assert_eq!(observer["run_path"], "claude-team");
    assert_eq!(observer["fields"]["model"], "sonnet");
    assert_eq!(
        observer["set_by"]["model"],
        "agents.configs.thread-observer"
    );
    assert_eq!(observer["fields"]["effort"], "medium");
    assert!(
        observer["set_by"]["effort"]
            .as_str()
            .unwrap()
            .ends_with("agents/thread-observer.md"),
        "{observer:#}"
    );
    assert_eq!(observer["fields"]["skills"][0], "dossier-practice");
    assert_eq!(observer["fields"]["budget"]["tokens_per_day"], 2_000_000);
    assert_eq!(observer["fields"]["budget"]["wake_tokens"], 40_000);
    assert_eq!(observer["set_by"]["budget"], "team vanilla");
    assert_eq!(observer["fields"]["may"].as_array().unwrap().len(), 7);
    assert_eq!(
        member(&vanilla, "ui0")["fields"]["may"],
        serde_json::json!(["note", "result"])
    );

    // a team of the repository that narrows passes; one that widens a tool list or the message kinds is refused
    write(
        &lab.repo.join(".legionctl/teams/narrow.jsonc"),
        r#"{ "name": "narrow", "members": {
            "main0": { "agent": "main-agent", "tools": { "allow": ["Read", "Bash"] } },
            "observer0": { "agent": "thread-observer", "budget": { "wake_tokens": 1000 } } } }"#,
    );
    let (narrow, ok) = lab.check(&["--team", "narrow"]);
    assert!(ok && refused(&narrow).is_empty(), "{narrow:#}");
    assert_eq!(
        member(&narrow, "main0")["fields"]["tools"]["allow"],
        serde_json::json!(["Read", "Bash"])
    );
    write(
        &lab.repo.join(".legionctl/teams/wide.jsonc"),
        r#"{ "name": "wide", "members": {
            "main0": { "agent": "main-agent", "tools": { "allow": ["Read", "Bash", "WebFetch"] } },
            "ui0": { "agent": "ui-composer" } },
            "flow": { "may": { "ui0": ["note", "hard-block"] } } }"#,
    );
    let (wide, ok) = lab.check(&["--team", "wide"]);
    assert!(!ok);
    assert_eq!(
        refused(&wide),
        [
            "main0: team wide widens tools.allow adds WebFetch",
            "ui0: flow.may widens: ui-composer may not send hard-block"
        ],
        "{wide:#}"
    );

    // the repository config layer may only narrow: removing the user's deny is refused
    write(
        &lab.repo.join(".legionctl/agents.jsonc"),
        r#"{ "agents": { "configs": { "main-agent": { "commands": { "deny": [] } } } } }"#,
    );
    let (repo, ok) = lab.check(&["--team", "vanilla"]);
    assert!(!ok);
    assert_eq!(
        refused(&repo),
        [
            "main0: the repository layer widens agents.configs.main-agent: commands.deny removes Bash(cargo publish*)"
        ]
    );
}

#[test]
fn a_field_that_a_run_path_cannot_apply_refuses_the_member_and_names_the_field() {
    let lab = Lab::new("paths", &shipped_plugin());
    lab.user_config(
        r#"{ "agents": { "configs": {
            "main-agent": { "sandbox": "read-only", "mcp": ["legionctl"] },
            "thread-observer": { "mcp": ["legionctl"], "sandbox": "workspace-write" }
        } } }"#,
    );
    let cut = |check: &Value| -> Vec<String> {
        refused(check)
            .iter()
            .map(|r| r.split(" (").next().unwrap().to_string())
            .collect()
    };
    // Claude: the lead is its own session (mcp applies; Claude has no sandbox); a teammate is a plugin agent type
    let (claude, ok) = lab.check(&["--team", "vanilla", "--harness", "claude"]);
    assert!(!ok);
    assert_eq!(
        cut(&claude),
        ["main0: sandbox", "observer0: mcp", "observer0: sandbox"],
        "{claude:#}"
    );
    let mcp = refused(&claude)
        .into_iter()
        .find(|r| r.starts_with("observer0: mcp"))
        .unwrap();
    assert!(
        mcp.contains("agents.configs.thread-observer")
            && mcp.contains("claude-team")
            && mcp.contains("mcpServers"),
        "{mcp}"
    );
    // Codex: the main session applies its sandbox; a Codex subagent inherits sandbox and MCP
    let (codex, ok) = lab.check(&["--team", "vanilla", "--harness", "codex"]);
    assert!(!ok);
    assert_eq!(member(&codex, "observer0")["run_path"], "codex-subagent");
    assert_eq!(
        cut(&codex),
        ["observer0: mcp", "observer0: sandbox"],
        "{codex:#}"
    );
}

#[test]
fn setup_apply_writes_each_agent_config_as_a_codex_agent_with_instructions_model_and_effort_only() {
    let plugin = shipped_plugin();
    let lab = Lab::new("codex", &plugin);
    lab.stand_in_ctx();
    write(&lab.home.join("rules.md"), "Local rules.\n");
    let agents = lab.home.join(".codex/agents");
    write(&agents.join("mine.toml"), "name = \"mine\"\n");
    write(
        &agents.join("gone.toml"),
        "# Written by `legionctl setup apply` from an old config\nname = \"gone\"\n",
    );
    let config = |extra: &str| {
        format!(
            r#"{{ "agents": {{ "configs": {{
                "thread-observer": {{ "model": "gpt-5.5-codex", "effort": "high" {extra} }},
                "reviewer": {{ "archetype": "thread-observer", "instructions": ["~/rules.md"] }},
                "claude-only": {{ "archetype": "ui-composer", "harness": "claude", "mcp": ["legionctl"] }}
            }} }} }}"#
        )
    };
    lab.user_config(&config(""));
    let out = lab.op.cli_in(&lab.home, &["setup", "apply"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(
        file_names(&agents),
        [
            "main-agent.toml",
            "mine.toml",
            "reviewer.toml",
            "thread-observer.toml",
            "ui-composer.toml"
        ]
    );
    let read = |name: &str| -> toml::Table {
        std::fs::read_to_string(agents.join(name))
            .unwrap()
            .parse()
            .unwrap()
    };
    let observer = read("thread-observer.toml");
    let keys: Vec<&String> = observer.keys().collect();
    assert_eq!(
        keys,
        [
            "description",
            "developer_instructions",
            "model",
            "model_reasoning_effort",
            "name"
        ]
    );
    assert_eq!(observer["model"].as_str(), Some("gpt-5.5-codex"));
    assert_eq!(observer["model_reasoning_effort"].as_str(), Some("high"));
    let text = observer["developer_instructions"].as_str().unwrap();
    assert!(
        text.contains("Test review") && text.contains("Backfill only while"),
        "{text}"
    );
    assert!(!text.contains("${CLAUDE_PLUGIN_ROOT}"), "{text}");
    let backfill = plugin.join("agent-refs/example-observer-backfill.md");
    assert!(text.contains(&backfill.display().to_string()), "{text}");
    assert!(text.contains("legionctl-dossier-practice"), "{text}");
    // the archetype sets no model: Codex keeps its own
    let main = read("main-agent.toml");
    assert!(!main.contains_key("model") && !main.contains_key("model_reasoning_effort"));
    let reviewer = read("reviewer.toml");
    assert!(
        reviewer["developer_instructions"]
            .as_str()
            .unwrap()
            .contains(&format!(
                "Read and follow {}",
                lab.home.join("rules.md").display()
            ))
    );
    assert_eq!(
        lab.setup_agent_items(),
        ["main-agent", "reviewer", "thread-observer", "ui-composer"]
            .map(|n| (format!("legionctl agent {n}"), "current".to_string()))
    );

    // a Codex member with its own sandbox: refused with the field named, and nothing written
    let before = std::fs::read_to_string(agents.join("thread-observer.toml")).unwrap();
    lab.user_config(&config(r#", "sandbox": "danger-full-access""#));
    let out = lab.op.cli_in(&lab.home, &["setup", "apply"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success()
            && err.contains("thread-observer: sandbox")
            && err.contains("codex-subagent"),
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(agents.join("thread-observer.toml")).unwrap(),
        before
    );
}

/// The operator runs checks with the PATH of the systemd user manager, which lacks the user's tool folders (todo-131):
/// `setup check` reports the environment.d file as missing or outdated, and `setup apply` writes it once. The real
/// environment.d generator, when the machine has it, reads the file as the folders before the manager's PATH.
#[test]
fn setup_apply_puts_the_users_tool_folders_before_the_user_managers_path_once() {
    let lab = Lab::new("path", &shipped_plugin());
    lab.stand_in_ctx();
    let conf = lab.home.join(".config/environment.d/50-legionctl.conf");
    let status = || -> String {
        let check: Value = serde_json::from_str(&lab.op.ok(&["setup", "check", "--json"])).unwrap();
        let item = check["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["what"] == "legionctl PATH")
            .unwrap_or_else(|| panic!("{check:#}"))
            .clone();
        assert_eq!(item["path"], conf.display().to_string());
        item["status"].as_str().unwrap().into()
    };
    let apply = || -> String {
        let out = lab.op.cli_in(&lab.home, &["setup", "apply"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let home = lab.home.display();
    let want = format!(
        "# Written by `legionctl setup apply`: the user's tool folders for the systemd user manager (the operator's checks)\n\
         PATH={home}/.local/bin:{home}/.cargo/bin:$PATH\n"
    );

    assert_eq!(status(), "missing");
    assert!(apply().contains(&format!("legionctl PATH: written to {}", conf.display())));
    assert_eq!(std::fs::read_to_string(&conf).unwrap(), want);
    assert_eq!(status(), "current");
    assert!(!apply().contains("legionctl PATH"), "written once");
    assert_eq!(file_names(conf.parent().unwrap()), ["50-legionctl.conf"]);

    write(&conf, "PATH=/old:$PATH\n");
    assert_eq!(status(), "outdated");
    apply();
    assert_eq!(std::fs::read_to_string(&conf).unwrap(), want);

    let generator = Path::new(
        "/usr/lib/systemd/user-environment-generators/30-systemd-environment-d-generator",
    );
    if generator.is_file() {
        let out = Command::new(generator)
            .env_clear()
            .env("PATH", "/usr/bin")
            .env("XDG_CONFIG_HOME", lab.home.join(".config"))
            .output()
            .unwrap();
        let env = String::from_utf8_lossy(&out.stdout);
        assert!(
            env.lines()
                .any(|l| l == format!("PATH={home}/.local/bin:{home}/.cargo/bin:/usr/bin")),
            "{env}"
        );
    }
}

#[test]
fn the_archetype_files_carry_only_the_frontmatter_fields_that_plugin_agents_honour() {
    let lab = Lab::new("files", &shipped_plugin());
    let (shipped, ok) = lab.check(&[]);
    assert!(ok && refused(&shipped).is_empty(), "{shipped:#}");
    let mut names: Vec<&str> = shipped["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["archetype"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["main-agent", "thread-observer", "ui-composer"]);
    drop(lab);

    // a copy with a field that plugin agents ignore, and a ref that is not there
    let copy = Scratch::new(format!(
        "/var/tmp/legionctl-agents-plugin-{}",
        std::process::id()
    ));
    assert!(
        Command::new("cp")
            .arg("-r")
            .arg(shipped_plugin().join("."))
            .arg(&copy)
            .status()
            .unwrap()
            .success()
    );
    let observer = copy.join("agents/thread-observer.md");
    let text = std::fs::read_to_string(&observer).unwrap();
    std::fs::write(
        &observer,
        text.replacen("effort: medium\n", "effort: medium\nmcpServers: [ctx]\n", 1),
    )
    .unwrap();
    let ui = copy.join("agents/ui-composer.md");
    let text = std::fs::read_to_string(&ui).unwrap();
    std::fs::write(
        &ui,
        text + "\n- `${CLAUDE_PLUGIN_ROOT}/agent-refs/missing.md` - read it.\n",
    )
    .unwrap();
    let lab = Lab::new("badfiles", &copy);
    let (bad, ok) = lab.check(&[]);
    assert!(!ok);
    let lines = refused(&bad);
    assert_eq!(lines.len(), 2, "{bad:#}");
    assert!(
        lines[0].ends_with("thread-observer.md: frontmatter field mcpServers is not one that plugin agents honour (name, description, tools, disallowedTools, model, effort, skills, background)"),
        "{lines:?}"
    );
    assert!(
        lines[1].ends_with("ui-composer.md: the ref agent-refs/missing.md does not exist"),
        "{lines:?}"
    );
}
