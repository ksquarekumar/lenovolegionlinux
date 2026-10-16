//! End-to-end: agent archetypes, configs and teams (design 23, B+2 to B+4, P6a) through the CLI and the real
//! operator: `runnables check` resolves the layers (archetype file, `agents.configs.<name>`, team) and refuses each field
//! that a member's run path cannot apply and each layer that widens; `setup apply` writes each config as a Codex agent
//! and the systemd user manager's PATH (environment.d). `agents accounts usage` reads each account's rate-limit
//! windows from stand-in providers with the profile's own token, read only (todo-136).

mod common;
#[path = "common/keyring.rs"]
mod keyring;
#[path = "common/usage.rs"]
mod usage;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::Operator;
use common::scratch::Scratch;
use serde_json::{Value, json};

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
                // a stand-in keyring: never the user's
                (
                    "PATH",
                    keyring::keyring_path(&home, &std::env::var_os("PATH").unwrap()),
                ),
                ("LEGIONCTL_PLUGIN", OsString::from(plugin)),
            ],
        );
        // never the machine's bun (`setup apply` installs the ACP adapters with it): a stand-in that logs each call to
        // ~/bun.log and keeps its global commands in ~/.cache/.bun/bin, as bun with XDG_CACHE_HOME does; before its
        // first `add -g` it has no global folder and `pm -g bin` fails as bun 1.x does
        let bun = home.join(".bun/bin/bun");
        write(
            &bun,
            r#"#!/bin/sh
echo "$*" >> "$HOME/bun.log"
global="$HOME/.cache/.bun/bin"
case "$1 $2" in
"pm -g")
  [ -d "$global" ] || { echo "error: No package.json was found for directory \"$HOME/.cache/.bun/install/global\"" >&2; exit 1; }
  echo "$global" ;;
"add -g")
  shift 2
  mkdir -p "$global"
  for p in "$@"; do printf '#!/bin/sh\n' > "$global/${p##*/}"; chmod +x "$global/${p##*/}"; done ;;
*) exit 64 ;;
esac
"#,
        );
        std::fs::set_permissions(&bun, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
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

    /// The JSON of `runnables check <args>` in the repository, and whether the text form exits 0.
    fn check(&self, args: &[&str]) -> (Value, bool) {
        let mut command = vec!["runnables", "check"];
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
            "main0": { "agent": "main-agent", "tools": { "allow": ["Read", "Bash"] }, "capabilities": ["web-read"] },
            "observer0": { "agent": "thread-observer", "budget": { "wake_tokens": 1000 } } } }"#,
    );
    let (narrow, ok) = lab.check(&["--team", "narrow"]);
    assert!(ok && refused(&narrow).is_empty(), "{narrow:#}");
    assert_eq!(
        member(&narrow, "main0")["fields"]["capabilities"],
        serde_json::json!(["web-read"])
    );
    assert_eq!(
        member(&narrow, "main0")["fields"]["tools"]["allow"],
        serde_json::json!(["Read", "Bash"])
    );
    write(
        &lab.repo.join(".legionctl/teams/wide.jsonc"),
        r#"{ "name": "wide", "members": {
            "main0": { "agent": "main-agent", "tools": { "allow": ["Read", "Bash", "WebFetch"] },
                       "capabilities": ["web-read", "rust-dev"] },
            "ui0": { "agent": "ui-composer" } },
            "flow": { "may": { "ui0": ["note", "hard-block"] } } }"#,
    );
    let (wide, ok) = lab.check(&["--team", "wide"]);
    assert!(!ok);
    assert_eq!(
        refused(&wide),
        [
            "main0: team wide widens tools.allow adds WebFetch",
            "main0: team wide widens capabilities: adds rust-dev",
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
    let conf = lab.home.join(".config/environment.d/99-legionctl.conf");
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
    // apply installs the ACP adapters of session.acp first: bun's global folder then exists
    let want = format!(
        "# Written by `legionctl setup apply`: the user's tool folders for the systemd user manager (the operator's checks)\n\
         PATH={home}/.local/bin:{home}/.cargo/bin:{home}/.cache/.bun/bin:$PATH\n"
    );

    assert_eq!(status(), "missing");
    assert!(apply().contains(&format!("legionctl PATH: written to {}", conf.display())));
    assert_eq!(std::fs::read_to_string(&conf).unwrap(), want);
    assert_eq!(status(), "current");
    assert!(!apply().contains("legionctl PATH"), "written once");
    assert_eq!(file_names(conf.parent().unwrap()), ["99-legionctl.conf"]);

    write(&conf, "PATH=/old:$PATH\n");
    assert_eq!(status(), "outdated");
    apply();
    assert_eq!(std::fs::read_to_string(&conf).unwrap(), want);

    let generator = Path::new(
        "/usr/lib/systemd/user-environment-generators/30-systemd-environment-d-generator",
    );
    if generator.is_file() {
        // a user file that sorts before ours sets PATH as a system file (Ubuntu's 90-*.conf) does: ours still wins
        write(
            &conf.with_file_name("90-system.conf"),
            "PATH=/usr/local/bin:/usr/bin\n",
        );
        let out = Command::new(generator)
            .env_clear()
            .env("PATH", "/usr/bin")
            .env("XDG_CONFIG_HOME", lab.home.join(".config"))
            .output()
            .unwrap();
        let env = String::from_utf8_lossy(&out.stdout);
        let path = env
            .lines()
            .rfind(|l| l.starts_with("PATH="))
            .unwrap_or_default();
        assert!(
            path.starts_with(&format!("PATH={home}/.local/bin:{home}/.cargo/bin:")),
            "{env}"
        );
    }
}

/// todo-144: the ACP adapters of `session.acp` (headless runs, the app's chat bar). `setup check` reports each adapter
/// command that is not on the user manager's PATH (with bun's global folder) as missing, with its package from
/// `session.acp_packages`; `setup apply` installs each with `bun add -g <package>` once, and puts bun's global folder
/// (`bun pm -g bin`, not on any PATH by itself) into the environment.d PATH file. An adapter with no package is
/// refused by apply, with its name.
#[test]
fn setup_check_reports_each_missing_acp_adapter_and_apply_installs_it_with_bun_once() {
    let lab = Lab::new("acp", &shipped_plugin());
    lab.stand_in_ctx();
    let adapters = || -> Vec<[String; 4]> {
        let check: Value = serde_json::from_str(&lab.op.ok(&["setup", "check", "--json"])).unwrap();
        check["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["what"].as_str().unwrap().starts_with("ACP adapter "))
            .map(|i| {
                ["what", "agent", "status", "path"].map(|k| i[k].as_str().unwrap().to_string())
            })
            .collect()
    };
    let apply = || lab.op.cli_in(&lab.home, &["setup", "apply"]);
    let installs = || -> Vec<String> {
        std::fs::read_to_string(lab.home.join("bun.log"))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with("add "))
            .map(String::from)
            .collect()
    };
    let g = lab.home.join(".cache/.bun/bin").display().to_string();
    let (claude, codex) = ("ACP adapter claude-code-acp", "ACP adapter codex-acp");

    assert_eq!(
        adapters(),
        [
            [
                claude,
                "claude",
                "missing",
                "@zed-industries/claude-code-acp"
            ],
            [codex, "codex", "missing", "@zed-industries/codex-acp"],
        ]
        .map(|row| row.map(String::from))
    );
    let text = String::from_utf8_lossy(&lab.op.cli(&["setup", "check"]).stdout).into_owned();
    assert!(
        text.lines().any(|l| l.starts_with(claude)
            && l.contains(" missing ")
            && l.ends_with("@zed-industries/claude-code-acp")),
        "{text}"
    );

    let out = apply();
    let done = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success()
            && done.contains(&format!(
                "{claude}: installed @zed-industries/claude-code-acp with bun add -g"
            ))
            && done.contains(&format!(
                "{codex}: installed @zed-industries/codex-acp with bun add -g"
            )),
        "{done}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        installs(),
        [
            "add -g @zed-industries/claude-code-acp",
            "add -g @zed-industries/codex-acp"
        ]
    );
    assert_eq!(
        adapters(),
        [
            [claude, "claude", "current", &format!("{g}/claude-code-acp")],
            [codex, "codex", "current", &format!("{g}/codex-acp")],
        ]
        .map(|row| row.map(String::from))
    );
    let conf =
        std::fs::read_to_string(lab.home.join(".config/environment.d/99-legionctl.conf")).unwrap();
    assert!(conf.contains(&format!(":{g}:$PATH\n")), "{conf}");
    assert!(
        !String::from_utf8_lossy(&apply().stdout).contains("ACP adapter"),
        "installed once"
    );
    assert_eq!(installs().len(), 2);

    // an adapter command that no package gives
    lab.user_config(r#"{"session": {"acp": {"local": ["local-acp"]}}}"#);
    let why = "missing (no package in session.acp_packages)";
    assert!(adapters().contains(&["ACP adapter local-acp", "local", why, ""].map(String::from)));
    let refused = apply();
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success()
            && stderr.contains(&format!("ACP adapter local-acp for local: {why}")),
        "{stderr}"
    );
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

/// F-9 (EV-review-05): a Codex config that is there but cannot be read (not UTF-8) is an error for `setup check` and
/// `setup apply`; apply never takes it as empty and never writes over it.
#[test]
fn setup_never_writes_over_a_codex_config_that_it_cannot_read() {
    let lab = Lab::new("codex-unreadable", &shipped_plugin());
    lab.stand_in_ctx();
    let config = lab.home.join(".codex/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    // Latin-1 "modèle": valid for the user's editor, not UTF-8
    let bytes = b"model = \"mod\xe8le\"\n".to_vec();
    std::fs::write(&config, &bytes).unwrap();

    let check: Value = serde_json::from_str(&lab.op.ok(&["setup", "check", "--json"])).unwrap();
    let item = check["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["what"] == "legionctl mcp" && i["agent"] == "codex")
        .unwrap_or_else(|| panic!("{check:#}"))
        .clone();
    assert!(
        item["status"].as_str().unwrap().contains("reading"),
        "{item:#}"
    );

    let out = lab.op.cli_in(&lab.home, &["setup", "apply"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && err.contains("config.toml"),
        "{err}"
    );
    assert_eq!(
        std::fs::read(&config).unwrap(),
        bytes,
        "the config keeps every byte"
    );
    assert!(!config.with_extension("toml.legionctl-bak").exists());
}

/// `setup check` reads ctx's own status: each agent's item from its JSON, and an output that is not that JSON is one
/// item whose status holds the output, not "nothing to report".
#[test]
fn setup_check_shows_ctx_status_and_the_output_that_it_cannot_read() {
    let lab = Lab::new("ctx-status", &shipped_plugin());
    write(
        &lab.home.join(".local/bin/ctx"),
        r#"#!/bin/sh
case "$1 $2 $3" in
"integrations status skill") echo '{"results":[{"agent":"claude-code","status":"current","path":"/h/.claude/skills/ctx"},{"agent":"codex","status":"missing","path":null}]}' ;;
"integrations status mcp") echo 'Error: config is locked' ;;
*) echo ctx 0.0.0 ;;
esac
"#,
    );
    std::fs::set_permissions(
        lab.home.join(".local/bin/ctx"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let check: Value = serde_json::from_str(&lab.op.ok(&["setup", "check", "--json"])).unwrap();
    let ctx: Vec<(&str, &str, &str)> = check["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["what"].as_str().unwrap().starts_with("ctx "))
        .map(|i| {
            (
                i["agent"].as_str().unwrap(),
                i["status"].as_str().unwrap(),
                i["path"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(ctx.len(), 3, "{check:#}");
    assert_eq!(ctx[0], ("claude-code", "current", "/h/.claude/skills/ctx"));
    assert_eq!(ctx[1], ("codex", "missing", ""));
    assert_eq!(ctx[2].0, "all");
    assert!(
        ctx[2]
            .1
            .contains("ctx integrations status mcp gave: Error: config is locked"),
        "{}",
        ctx[2].1
    );
}

/// An account usage lab: the stand-in usage endpoint of both providers (`usage_ttl_s` 3600) and four profiles:
/// work (Claude in its profile, Codex at rest in the keyring), spare (both in its profile), old (an expired Claude
/// token, a Codex token the provider refuses) and changed (an answer of a changed shape). `files` are the login
/// files as written.
struct UsageLab {
    lab: Lab,
    answers: PathBuf,
    files: Vec<(PathBuf, String)>,
    _server: common::process::Kill,
}

impl UsageLab {
    fn new() -> UsageLab {
        let lab = Lab::new("usage", &shipped_plugin());
        let answers = lab.home.join("usage");
        let (server, port) = usage::usage_server(&answers);
        let url = |agent: &str| format!("http://127.0.0.1:{port}/{agent}");
        lab.user_config(
            &json!({"agents": {"accounts": {"usage_ttl_s": 3600,
                "endpoints": {"claude": url("claude"), "codex": url("codex")}}}})
            .to_string(),
        );
        for name in ["work", "spare", "old", "changed"] {
            lab.op.ok(&["agents", "accounts", "add", name]);
        }
        let soon = "4102444800000"; // 2100-01-01 in ms
        let claude = |token: &str, expires: &str| {
            format!(
                r#"{{"claudeAiOauth": {{"accessToken": "{token}", "refreshToken": "r-{token}", "expiresAt": {expires}}}}}"#
            )
        };
        let codex = |token: &str| format!(r#"{{"tokens": {{"access_token": "{token}"}}}}"#);
        let mut files = Vec::new();
        for (account, agent, text) in [
            ("work", "claude", claude("tok-work", soon)),
            ("spare", "claude", claude("tok-spare", soon)),
            ("spare", "codex", codex("tok-sparex")),
            ("old", "claude", claude("tok-old", "1000")),
            ("old", "codex", codex("tok-oldx")),
            ("changed", "claude", claude("tok-changed", soon)),
        ] {
            let file = if agent == "claude" {
                ".credentials.json"
            } else {
                "auth.json"
            };
            let path = lab
                .home
                .join(".local/share/legionctl/accounts")
                .join(account)
                .join(agent)
                .join(file);
            write(&path, &text);
            files.push((path, text));
        }
        // work's Codex login rests in the keyring (no session of the account runs)
        write(
            &lab.home.join("keyring/work_codex"),
            r#"{"tokens": {"access_token": "tok-workx", "account_id": "acct-1"}}"#,
        );
        let u = UsageLab {
            lab,
            answers,
            files,
            _server: server,
        };
        u.answer(
            "tok-work",
            &json!({"body": {
            "five_hour": {"utilization": 42.0, "resets_at": "2026-10-17T05:00:00.512+00:00"},
            "seven_day": {"utilization": 7.5, "resets_at": null}}}),
        );
        u.answer(
            "tok-spare",
            &json!({"body": {"limits": [{"kind": "weekly_all", "percent": 80.0},
            {"kind": "session", "percent": 3.0, "resets_at": "2026-10-17T10:30:00Z"}],
            "five_hour": {"utilization": 99.0}, "seven_day": {"utilization": 99.0}}}),
        );
        u.answer("tok-workx", &json!({"body": {"rate_limit": {"limit_reached": true,
            "primary_window": {"used_percent": 97.0, "limit_window_seconds": 18_000, "reset_at": 1_800_000_000_i64},
            "secondary_window": {"used_percent": 20.0, "limit_window_seconds": 604_800}}}}));
        u.answer(
            "tok-sparex",
            &json!({"body": {"plan_type": "plus"}, "headers": {
            "x-codex-primary-used-percent": "12.5", "x-codex-primary-window-minutes": "300",
            "x-codex-primary-reset-at": "1800000600", "x-codex-secondary-used-percent": "61",
            "x-codex-secondary-window-minutes": "10080"}}),
        );
        u.answer(
            "tok-oldx",
            &json!({"status": 401, "body": {"error": "token expired"}}),
        );
        u.answer(
            "tok-changed",
            &json!({"body": {"five_hour": {"used": 3}, "seven_day": {"utilization": 1.0}}}),
        );
        u
    }

    /// The provider answers `token` with `answer` from now on.
    fn answer(&self, token: &str, answer: &Value) {
        write(
            &self.answers.join(format!("{token}.json")),
            &answer.to_string(),
        );
    }

    /// `agents accounts usage --json <args>`.
    fn read(&self, args: &[&str]) -> Value {
        let mut command = vec!["agents", "accounts", "usage", "--json"];
        command.extend(args);
        serde_json::from_str(&self.lab.op.ok(&command)).unwrap()
    }

    /// The calls the provider got.
    fn calls(&self) -> Vec<Value> {
        std::fs::read_to_string(self.answers.join("calls.log"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

/// The row of `account` and `agent` in a usage answer.
fn row(usage: &Value, account: &str, agent: &str) -> Value {
    usage
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["account"] == account && r["agent"] == agent)
        .cloned()
        .unwrap_or_else(|| panic!("no {account}/{agent} in {usage:#}"))
}

/// The windows of a row that has no error.
fn windows(usage: &Value, account: &str, agent: &str) -> Value {
    let r = row(usage, account, agent);
    assert!(r["error"].is_null(), "{account}/{agent}: {r}");
    r["windows"].clone()
}

/// `agents accounts usage` reads each logged-in agent of each profile from its provider (both stand-ins) with the
/// profile's own token, the way the agents' own clients ask (S1 of planning/features/accounts.jsonc): Claude's
/// top-level windows and its `limits[]` form, Codex's body (a reached limit counts as 100%) and its rate-limit
/// headers only (V3); a login at rest in the keyring counts. An expired Claude token is "login needed" and is never
/// sent (V1); a refused token is "login needed" too; a changed answer is that account's error with the field, no zero
/// use (V2). No token is printed, no login file or keyring entry changes. The answers are kept with their fetch time;
/// `--fresh` reads again; the seed view shows them by account.
#[test]
fn account_usage_comes_from_each_provider_read_only_and_a_changed_answer_is_an_error() {
    let u = UsageLab::new();
    let usage = u.read(&[]);
    assert_eq!(
        usage.as_array().unwrap().len(),
        7,
        "each logged-in agent once: {usage:#}"
    );
    assert_eq!(
        windows(&usage, "work", "claude"),
        json!([{"name": "5h", "used_percent": 42.0, "resets_at": 1_792_213_200_i64},
               {"name": "7d", "used_percent": 7.5, "resets_at": null}])
    );
    assert_eq!(
        windows(&usage, "spare", "claude"),
        json!([{"name": "5h", "used_percent": 3.0, "resets_at": 1_792_233_000_i64},
               {"name": "7d", "used_percent": 80.0, "resets_at": null}]),
        "limits[] is the source where it has the window"
    );
    assert_eq!(
        windows(&usage, "work", "codex"),
        json!([{"name": "5h", "used_percent": 100.0, "resets_at": 1_800_000_000_i64},
               {"name": "7d", "used_percent": 20.0, "resets_at": null}]),
        "a reached limit counts the fuller window as 100%"
    );
    assert_eq!(
        windows(&usage, "spare", "codex"),
        json!([{"name": "5h", "used_percent": 12.5, "resets_at": 1_800_000_600_i64},
               {"name": "7d", "used_percent": 61.0, "resets_at": null}]),
        "the rate-limit headers when the body has no rate_limit"
    );
    for (account, agent, text) in [
        (
            "old",
            "claude",
            "login needed: the access token of account old expired",
        ),
        ("old", "codex", "login needed: http://127.0.0.1"),
        (
            "changed",
            "claude",
            "five_hour: the answer has no number utilization",
        ),
    ] {
        let r = row(&usage, account, agent);
        let error = r["error"].as_str().unwrap_or_default();
        assert!(
            error.contains(text) && r["windows"] == json!([]),
            "{account}/{agent}: {r}"
        );
    }
    each_call_is_the_agents_own_and_reads_only(&u, &usage);
    the_answer_is_kept_until_fresh(&u, &usage);
}

/// Each call asks as the agent's own client (Claude's beta header, Codex's account id), an expired token is never
/// sent, no login file or keyring entry changes, no answer shows a token, and a login at rest in the keyring counts.
fn each_call_is_the_agents_own_and_reads_only(u: &UsageLab, usage: &Value) {
    let calls = u.calls();
    assert!(
        calls.iter().all(|c| c["token"] != "tok-old"),
        "an expired token is never sent: {calls:?}"
    );
    let call = |token: &str| calls.iter().find(|c| c["token"] == token).unwrap().clone();
    assert_eq!(
        [&call("tok-work")["path"], &call("tok-work")["beta"]],
        [&json!("/claude"), &json!("oauth-2025-04-20")]
    );
    assert_eq!(
        [&call("tok-workx")["path"], &call("tok-workx")["account"]],
        [&json!("/codex"), &json!("acct-1")]
    );
    for (path, text) in &u.files {
        assert_eq!(
            &std::fs::read_to_string(path).unwrap(),
            text,
            "{}",
            path.display()
        );
    }
    let home = &u.lab.home;
    assert!(
        !home
            .join(".local/share/legionctl/accounts/work/codex/auth.json")
            .exists()
    );
    assert_eq!(
        std::fs::read_dir(home.join("keyring")).unwrap().count(),
        1,
        "nothing went into the keyring"
    );
    let text = u.lab.op.ok(&["agents", "accounts", "usage"]);
    assert!(
        !text.contains("tok-") && !usage.to_string().contains("tok-"),
        "{text}"
    );
    assert!(text.contains("work") && text.contains("5h 42%"), "{text}");
    let list: Value =
        serde_json::from_str(&u.lab.op.ok(&["agents", "accounts", "list", "--json"])).unwrap();
    let work = list
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "work")
        .unwrap();
    assert_eq!(
        [&work["claude"], &work["codex"]],
        [&json!(true), &json!(true)],
        "a login at rest in the keyring is a login: {work}"
    );
}

/// The kept answer, with its fetch time, until `--fresh`; the seed view groups the accounts.
fn the_answer_is_kept_until_fresh(u: &UsageLab, usage: &Value) {
    u.answer(
        "tok-work",
        &json!({"body": {"five_hour": {"utilization": 88.0}, "seven_day": {"utilization": 9.0}}}),
    );
    let again = u.read(&["--account", "work"]);
    assert_eq!(
        row(&again, "work", "claude"),
        row(usage, "work", "claude"),
        "the kept answer and its fetch time"
    );
    let fresh = u.read(&["--account", "work", "--fresh"]);
    assert_eq!(windows(&fresh, "work", "claude")[0]["used_percent"], 88.0);
    let seed = u.lab.op.ok(&["views", "render", "accounts"]);
    for line in [
        "== work (2) ==",
        "claude | 5h | 88 | 7d | 9 | no value | now",
        "codex | 5h | 100 | 7d | 20 | no value | now",
        "claude | no value | no value | no value | no value | five_hour: the answer has no number utilization | now",
    ] {
        assert!(seed.lines().any(|l| l == line), "{line} missing:\n{seed}");
    }
}

/// The plugin's text names only commands that this build has (todo-176 S5: a plugin of a newer tree named
/// `dossier health`, which the installed binary did not know). Each `` `legionctl <words> ...` `` in a skill, ref,
/// command or agent file: its words up to the first placeholder, option or value give a command that `--help` knows.
#[test]
fn the_plugin_text_names_only_commands_of_this_build() {
    fn files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files(&path, out);
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
    }
    let mut found = Vec::new();
    files(&shipped_plugin(), &mut found);
    let mut commands = std::collections::BTreeSet::new();
    for file in &found {
        let text = std::fs::read_to_string(file).unwrap();
        for quoted in text.split('`').skip(1).step_by(2) {
            let Some(rest) = quoted.strip_prefix("legionctl ") else {
                continue;
            };
            // a word is a subcommand while it is lower case letters and '-'
            let words: Vec<&str> = rest
                .split_whitespace()
                .take_while(|w| {
                    w.chars().all(|c| c.is_ascii_lowercase() || c == '-') && !w.starts_with('-')
                })
                .collect();
            if !words.is_empty() {
                commands.insert(words.join(" "));
            }
        }
    }
    assert!(commands.len() > 30, "{commands:?}");
    // the help of the longest known prefix: the whole command, or a leaf whose further words are values
    let help = |words: &[&str]| -> Option<String> {
        let out = Command::new(env!("CARGO_BIN_EXE_legionctl"))
            .args(words)
            .arg("--help")
            .env_clear()
            .output()
            .unwrap();
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let unknown: Vec<&String> = commands
        .iter()
        .filter(|command| {
            let words: Vec<&str> = command.split(' ').collect();
            !(1..=words.len())
                .rev()
                .find_map(|n| help(&words[..n]).map(|h| (n, h)))
                .is_some_and(|(n, h)| n == words.len() || !h.contains("Commands:"))
        })
        .collect();
    assert!(
        unknown.is_empty(),
        "plugin text names commands that this build does not have: {unknown:?}"
    );
}
