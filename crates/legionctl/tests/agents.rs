//! End-to-end: agent archetypes, configs and teams (design 23, B+2 to B+4, P6a) through the CLI and the real
//! operator: `runnables check` resolves the layers (archetype file, `agents.configs.<name>`, team) and refuses each field
//! that a member's run path cannot apply and each layer that widens; `setup apply` writes each config as a Codex agent
//! and the systemd user manager's PATH (environment.d). `agents accounts usage` reads each account's rate-limit
//! windows from stand-in providers with the profile's own token, read only (todo-136).

#[path = "common/clauth.rs"]
mod clauth;
mod common;
#[path = "common/keyring.rs"]
mod keyring;
#[path = "common/plain.rs"]
mod plain;
#[path = "common/trust.rs"]
mod trust;

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

/// Never a real cargo install: a stand-in cargo puts the stand-in clauth in place and logs its calls (the stand-in
/// systemctl starts clauth's daemon unit: a file).
fn stand_in_clauth_setup(home: &Path) {
    write(&home.join("clauth-template"), clauth::SCRIPT);
    std::fs::set_permissions(
        home.join("clauth-template"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let cargo = home.join(".cargo/bin/cargo");
    write(
        &cargo,
        "#!/bin/sh\necho \"$*\" >> \"$HOME/cargo.log\"\n[ \"$1 $2\" = \"install clauth\" ] || exit 64\ncp \"$HOME/clauth-template\" \"$HOME/.cargo/bin/clauth\"\n",
    );
    std::fs::set_permissions(&cargo, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
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
                    // the system's folders only: the user's own tool folders (~/.cache/.bun/bin) hold the real ACP
                    // adapters, which setup would find instead of the stand-in bun's
                    keyring::keyring_path(&home, &system_path()),
                ),
                ("LEGIONCTL_PLUGIN", OsString::from(plugin)),
            ],
        );
        // never the machine's bun (`setup apply` installs the ACP adapters with it): a stand-in that logs each call to
        // ~/bun.log and links its global commands in $BUN_INSTALL/bin (else ~/.cache/.bun/bin, as bun with the user's
        // XDG_CACHE_HOME does) to the package's folder; before its first `add -g` it has no global folder and `pm -g
        // bin` fails as bun 1.x does
        let bun = home.join(".bun/bin/bun");
        write(
            &bun,
            r#"#!/bin/sh
echo "$*" >> "$HOME/bun.log"
root="${BUN_INSTALL:-$HOME/.cache/.bun}"
global="$root/bin"
case "$1 $2" in
"pm -g")
  [ -f "$root/install/global/package.json" ] || { echo "error: No package.json was found for directory \"$root/install/global\"" >&2; exit 1; }
  echo "$global" ;;
"add -g")
  shift 2
  mkdir -p "$global" "$root/install/global"
  touch "$root/install/global/package.json"
  for p in "$@"; do
    dir="$root/install/global/node_modules/$p"; mkdir -p "$dir"
    printf '#!/bin/sh\n' > "$dir/index.js"; chmod +x "$dir/index.js"; ln -sfn "$dir/index.js" "$global/${p##*/}"
  done ;;
*) exit 64 ;;
esac
"#,
        );
        std::fs::set_permissions(&bun, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        // never the machine's Claude Code: a stand-in `claude plugin` that logs each call to ~/claude.log and keeps
        // its marketplace and install records where Claude Code keeps them
        let claude = home.join(".local/bin/claude");
        write(
            &claude,
            r#"#!/bin/sh
echo "$*" >> "$HOME/claude.log"
plugins="$HOME/.claude/plugins"
mkdir -p "$plugins"
case "$1 $2 $3" in
"plugin marketplace add") printf '{"lenovolegionlinux": {"source": {"source": "directory", "path": "%s"}}}' "$4" > "$plugins/known_marketplaces.json" ;;
"plugin marketplace remove") rm "$plugins/known_marketplaces.json" ;;
"plugin install legionctl@lenovolegionlinux")
  market=$(sed 's/.*"path": "\([^"]*\)".*/\1/' "$plugins/known_marketplaces.json")
  printf '{"version": 2, "plugins": {"legionctl@lenovolegionlinux": [{"scope": "user", "installPath": "%s/agent-plugin"}]}}' "$market" > "$plugins/installed_plugins.json" ;;
"mcp add --scope") printf '{"mcpServers": {"clauth": {"command": "%s", "args": ["mcp"]}}}' "$7" > "$HOME/.claude.json" ;;
*) exit 64 ;;
esac
"#,
        );
        std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        // never the desktop's GNOME Shell: a stand-in `gnome-extensions` whose enabled state is ~/osd-enabled
        let extensions = home.join(".local/bin/gnome-extensions");
        write(
            &extensions,
            r#"#!/bin/sh
echo "$*" >> "$HOME/gnome-extensions.log"
case "$1" in
info) [ -e "$HOME/osd-enabled" ] && echo "  Enabled: Yes" || echo "  Enabled: No" ;;
enable) touch "$HOME/osd-enabled" ;;
*) exit 64 ;;
esac
"#,
        );
        std::fs::set_permissions(
            &extensions,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        // never the machine's Codex: a stand-in `codex` with codex 0.163's plugin behaviour (tests/fixtures/fake-codex.py)
        let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-codex.py");
        let codex = home.join(".bun/bin/codex");
        write(
            &codex,
            &format!("#!/bin/sh\nexec python3 {} \"$@\"\n", fake.display()),
        );
        std::fs::set_permissions(&codex, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        stand_in_systemctl(&home);
        stand_in_clauth_setup(&home);
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
    trust::allow(&lab.home, &lab.repo.join(".legionctl/teams/narrow.jsonc"));
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
    trust::allow(&lab.home, &lab.repo.join(".legionctl/teams/wide.jsonc"));
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
    trust::allow(&lab.home, &lab.repo.join(".legionctl/agents.jsonc"));
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
         PATH={home}/.local/bin:{home}/.cargo/bin:{home}/.bun/bin:$PATH\n"
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

/// The `add` calls of the stand-in bun in `home`.
fn bun_installs(home: &Path) -> Vec<String> {
    std::fs::read_to_string(home.join("bun.log"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with("add "))
        .map(String::from)
        .collect()
}

/// r324's codex-acp in `home`: Zed's package, which gives the same command (its Codex 0.137 runs no model of a
/// `ChatGPT` account today), linked as bun links it.
fn zed_codex_acp(home: &Path) {
    let zed =
        home.join(".bun/install/global/node_modules/@zed-industries/codex-acp/bin/codex-acp.js");
    write(&zed, "#!/bin/sh\n");
    std::fs::set_permissions(&zed, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(home.join(".bun/bin")).unwrap();
    std::os::unix::fs::symlink(&zed, home.join(".bun/bin/codex-acp")).unwrap();
    write(&home.join(".bun/install/global/package.json"), "{}");
}

/// todo-144: the ACP adapters of `session.acp` (headless runs, the app's chat bar; the defaults run each with
/// `bun --bun x --no-install <adapter>`). `setup check` reports each adapter that is not in bun's global folder or on
/// the user manager's PATH as missing, with its package from `session.acp_packages`, and a command that another
/// package gives (r324's Zed codex-acp) as from another package; `setup apply` installs each with `bun add -g
/// <package>` once, and puts bun's global folder
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
    let installs = || bun_installs(&lab.home);
    let g = lab.home.join(".bun/bin").display().to_string();
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
            [codex, "codex", "missing", "@agentclientprotocol/codex-acp"],
        ]
        .map(|row| row.map(String::from))
    );
    zed_codex_acp(&lab.home);
    let other = [
        codex,
        "codex",
        "from another package",
        "@agentclientprotocol/codex-acp",
    ];
    assert!(
        adapters().contains(&other.map(String::from)),
        "{:?}",
        adapters()
    );
    let text = String::from_utf8_lossy(&lab.op.cli(&["setup", "check"]).stdout).into_owned();
    assert!(
        text.lines().any(|l| l.starts_with(claude)
            && l.contains(" missing ")
            && l.ends_with("@zed-industries/claude-code-acp")),
        "{text}"
    );

    plain::assert_plain("setup check", &text);
    let out = apply();
    let done = String::from_utf8_lossy(&out.stdout);
    plain::assert_plain("setup apply", &done);
    assert!(
        out.status.success()
            && done.contains(&format!(
                "{claude}: installed @zed-industries/claude-code-acp with bun add -g"
            ))
            && done.contains(&format!(
                "{codex}: installed @agentclientprotocol/codex-acp with bun add -g"
            )),
        "{done}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        installs(),
        [
            "add -g @zed-industries/claude-code-acp",
            "add -g @agentclientprotocol/codex-acp"
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

/// Never the machine's services: a stand-in `systemctl` in `home`'s ~/.local/bin whose units run while ~/<unit>
/// exists (both at first).
fn stand_in_systemctl(home: &Path) {
    let systemctl = home.join(".local/bin/systemctl");
    write(
        &systemctl,
        r#"#!/bin/sh
echo "$*" >> "$HOME/systemctl.log"
[ "$1" = --user ] && shift
case "$1" in
is-active) [ -e "$HOME/$2" ] && echo active || { echo inactive; exit 3; } ;;
enable) [ "$2" = --now ] && touch "$HOME/$3" ;;
*) exit 64 ;;
esac
"#,
    );
    std::fs::set_permissions(
        &systemctl,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    for unit in ["legionctl-system.service", "legionctl-operator.socket"] {
        write(&home.join(unit), "");
    }
}

/// U-14, user decision 2026-10-17: the package turns the services on, and the user can turn them off. `setup check`
/// has a row for each with its state and the line that turns it on. `setup turn-on <service>` (Health's Turn on) runs
/// only that service's step, also with ctx missing; `setup apply` turns on only the ones that are off
/// (`systemctl enable --now`; polkit asks for legionctl-system), and then both are current.
#[test]
fn setup_check_shows_each_service_that_is_off_and_apply_turns_it_on() {
    let lab = Lab::new("services", &shipped_plugin());
    std::fs::remove_file(lab.home.join("legionctl-system.service")).unwrap();
    let rows = || -> Vec<(String, String, String)> {
        let check: Value = serde_json::from_str(&lab.op.ok(&["setup", "check", "--json"])).unwrap();
        check["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["agent"] == "machine" || i["agent"] == "user")
            .map(|i| {
                let field = |k: &str| i[k].as_str().unwrap().to_string();
                (field("what"), field("status"), field("path"))
            })
            .collect()
    };
    let row = |what: &str, status: &str, path: &str| {
        (what.to_string(), status.to_string(), path.to_string())
    };
    assert_eq!(
        rows(),
        [
            row(
                "legionctl-system",
                "off",
                "sudo systemctl enable --now legionctl-system.service"
            ),
            row(
                "legionctl-operator",
                "current",
                "systemctl --user enable --now legionctl-operator.socket"
            ),
        ]
    );
    let log = || std::fs::read_to_string(lab.home.join("systemctl.log")).unwrap();
    let _ = std::fs::remove_file(lab.home.join("systemctl.log"));
    let refused = lab.op.cli(&["setup", "turn-on", "sshd"]);
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains(
            "sshd is not a service of legionctl; the services are legionctl-system, legionctl-operator"
        ),
        "{refused:?}"
    );
    // no ctx here: its own step only, not the agent setup
    let out = lab.op.cli(&["setup", "turn-on", "legionctl-system"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        log(),
        "is-active legionctl-system.service\nenable --now legionctl-system.service\n"
    );
    assert!(rows().iter().all(|(_, status, _)| status == "current"));

    lab.stand_in_ctx();
    std::fs::remove_file(lab.home.join("legionctl-system.service")).unwrap();
    std::fs::remove_file(lab.home.join("systemctl.log")).unwrap();
    let out = lab.op.cli_in(&lab.home, &["setup", "apply"]);
    let done = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && done.contains("legionctl-system: turned on"),
        "{done}"
    );
    assert_eq!(
        log(),
        "is-active legionctl-system.service\nenable --now legionctl-system.service\n\
         --user is-active legionctl-operator.socket\n--user enable --now legionctl-clauth.service\n",
        "only the services that are off (clauth's daemon too: Set up installs clauth)"
    );
    assert!(rows().iter().all(|(_, status, _)| status == "current"));
}

/// U-14: the Claude Code plugin and the OSD extension are part of the setup, not manual steps that `setup check` does
/// not see. With Claude Code and no plugin, check has a missing row; apply installs it from the package's folder and
/// enables the OSD; a marketplace of the same name from a checkout (the 2026-10-17 incident's cause) is not of this
/// build, and apply replaces it.
#[test]
fn setup_check_sees_the_claude_code_plugin_and_apply_installs_it_from_the_package() {
    let lab = Lab::new("cc-plugin", &shipped_plugin());
    lab.stand_in_ctx();
    let row = || -> String {
        let check: Value = serde_json::from_str(&lab.op.ok(&["setup", "check", "--json"])).unwrap();
        let rows: Vec<&Value> = check["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["what"] == "legionctl plugin" && i["agent"] == "claude-code")
            .collect();
        assert_eq!(rows.len(), 1, "{check}");
        rows[0]["status"].as_str().unwrap().to_string()
    };
    let apply = || {
        let out = lab.op.cli_in(&lab.home, &["setup", "apply"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let calls = || std::fs::read_to_string(lab.home.join("claude.log")).unwrap_or_default();
    let package = shipped_plugin().parent().unwrap().to_path_buf();

    let osd = || -> String {
        let check: Value = serde_json::from_str(&lab.op.ok(&["setup", "check", "--json"])).unwrap();
        check["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["what"] == "OSD extension")
            .map(|i| i["status"].as_str().unwrap().to_string())
            .unwrap_or_default()
    };
    assert_eq!((row().as_str(), osd().as_str()), ("missing", "not enabled"));
    let done = apply();
    assert!(
        done.contains("legionctl plugin: installed into Claude Code from")
            && done.contains("OSD extension: enabled (it shows at the next login)"),
        "{done}"
    );
    assert_eq!(osd(), "current");
    assert_eq!(
        calls(),
        format!(
            "plugin marketplace add {}\nplugin install legionctl@lenovolegionlinux\nmcp add --scope user clauth -- {} \
             mcp\n",
            package.display(),
            lab.home.join(".cargo/bin/clauth").display()
        )
    );
    assert_eq!(row(), "current");
    let again = apply();
    assert!(
        !again.contains("legionctl plugin"),
        "nothing to do once current: {again}"
    );

    // a checkout as the marketplace: its plugin can be of another build
    let checkout = lab.home.join("checkout");
    write(
        &checkout.join(".claude-plugin/marketplace.json"),
        r#"{"name": "lenovolegionlinux", "plugins": [{"name": "legionctl", "source": "./agent-plugin"}]}"#,
    );
    write(
        &checkout.join("agent-plugin/hooks/hooks.json"),
        r#"{"hooks": {}}"#,
    );
    write(
        &lab.home.join(".claude/plugins/known_marketplaces.json"),
        &serde_json::json!({"lenovolegionlinux": {"source": {"source": "directory", "path": checkout}}}).to_string(),
    );
    assert_eq!(row(), "not of this build");
    std::fs::remove_file(lab.home.join("claude.log")).unwrap();
    apply();
    assert_eq!(
        calls(),
        format!(
            "plugin marketplace remove lenovolegionlinux\nplugin marketplace add {}\nplugin install \
             legionctl@lenovolegionlinux\n",
            package.display()
        )
    );
    assert_eq!(row(), "current");
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

/// An accounts lab: the stand-in clauth (`common/clauth.rs`) and four clauth profiles laid out as clauth 0.17.0 keeps them
/// (`profiles.toml`, `codex-profiles.toml`, `profiles/<name>/credentials.json` and `auth.json`): work, spare, old and
/// changed, each with a Claude Code and a Codex login of its own refresh token.
struct AccountsLab {
    lab: Lab,
}

impl AccountsLab {
    fn new(tag: &str) -> AccountsLab {
        let lab = Lab::new(tag, &shipped_plugin());
        let home = lab.home.to_path_buf();
        clauth::install(&home);
        let roster = "profiles = [\"work\", \"spare\", \"old\", \"changed\"]\n";
        write(&home.join(".clauth/profiles.toml"), roster);
        write(&home.join(".clauth/codex-profiles.toml"), roster);
        for name in ["work", "spare", "old", "changed"] {
            write(
                &home.join(format!(".clauth/profiles/{name}/credentials.json")),
                &json!({"claudeAiOauth": {"accessToken": format!("tok-{name}"), "refreshToken": format!("r-{name}")}})
                    .to_string(),
            );
            write(
                &home.join(format!(".clauth/profiles/{name}/auth.json")),
                &json!({"tokens": {"access_token": format!("tok-{name}x"), "refresh_token": format!("r-{name}x")}})
                    .to_string(),
            );
        }
        AccountsLab { lab }
    }

    /// `agents accounts usage --json <args>`.
    fn read(&self, args: &[&str]) -> Value {
        let mut command = vec!["agents", "accounts", "usage", "--json"];
        command.extend(args);
        serde_json::from_str(&self.lab.op.ok(&command)).unwrap()
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

/// todo-188: the use of each account is clauth's (its status feed, which its daemon writes), never a read of
/// legionctl's own. Each login and agent is one plain sentence with clauth's state of it: the windows, an expired
/// login (clauth renews it), the place in clauth's fallback chain and its threshold, a live clauth session, when it
/// was read; a refused login, a profile clauth does not show, no daemon and a use not read yet say so in words. The
/// CLI text and the seed view hold no error or builder text and no token. A feed of another schema is one error that
/// names both versions.
#[test]
fn account_use_comes_from_clauths_status_feed_in_one_plain_sentence() {
    use clauth::profile;
    let a = AccountsLab::new("usage");
    let home = a.lab.home.to_path_buf();
    let mut live = profile(
        "work",
        "claude",
        false,
        &[("5h", 42.0), ("7d", 7.0)],
        Some((1, 90.0)),
    );
    live["has_live_session"] = json!(true);
    let mut expired = profile("spare", "claude", false, &[("5h", 3.0), ("7d", 80.0)], None);
    expired["auth_status"] = json!("expired");
    let mut broken = profile("old", "claude", false, &[], None);
    broken["auth_status"] = json!("broken");
    let feed = [
        live,
        expired,
        broken,
        profile("changed", "claude", false, &[], None),
        profile("work", "codex", false, &[("5h", 100.0), ("7d", 20.0)], None),
        profile("old", "codex", false, &[], None),
        profile("changed", "codex", false, &[], None),
    ];
    clauth::status(&home, &feed);
    let usage = a.read(&[]);
    let text = |account: &str, agent: &str| {
        row(&usage, account, agent)["text"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(
        usage.as_array().unwrap().len(),
        8,
        "each login once: {usage:#}"
    );
    assert_eq!(
        text("work", "claude"),
        "Claude Code: 5h 42% used, 7d 7% used. In clauth's fallback chain at 1: it moves on at 90%. A clauth session \
         runs on it. Read just now."
    );
    assert_eq!(
        text("spare", "claude"),
        "Claude Code: 5h 3% used, 7d 80% used. The login has expired; clauth renews it at its next use. Read just now."
    );
    assert_eq!(
        text("work", "codex"),
        "Codex: 5h 100% used, 7d 20% used. Read just now."
    );
    assert_eq!(
        text("old", "claude"),
        "Claude Code: the provider refused the login of old; log in to it again."
    );
    assert_eq!(
        text("spare", "codex"),
        "Codex: clauth does not show spare (it is turned off in clauth)."
    );
    no_daemon_starts_with_its_button_and_sessions_show_local_time(&a, &usage);
    let cli = a.lab.op.ok(&["agents", "accounts", "usage"]);
    let seed = a.lab.op.ok(&["views", "render", "accounts"]);
    // the seed's terminal form ends with its actions for agents (operation names): the rows are the user's
    let rows: String = seed
        .lines()
        .filter(|l| !l.starts_with("actions:"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !usage.to_string().contains("tok-"),
        "no token in the answer"
    );
    for shown in [&cli, &rows] {
        plain::assert_plain("the accounts", shown);
        assert!(
            !shown.contains("tok-") && !shown.contains("r-work"),
            "{shown}"
        );
    }
    assert!(
        seed.lines().any(|l| l == "== work (2) =="),
        "grouped by account:\n{seed}"
    );

    another_schema_is_one_error(&a);
}

/// No daemon (lead walk r361): the action is the message, a Start clauth button on the row and no sentence about the
/// mechanism; the CLI line names the same step, and the step starts clauth's daemon. Claude Code's sessions as clauth
/// lists them show the time in local time (not clauth's UTC text) and no account column (clauth names none for a
/// session it did not start).
fn no_daemon_starts_with_its_button_and_sessions_show_local_time(a: &AccountsLab, usage: &Value) {
    let home = a.lab.home.to_path_buf();
    assert_eq!(
        row(usage, "changed", "claude")["text"],
        "Claude Code: use unknown."
    );
    let start = &row(usage, "changed", "claude")["actions"][0];
    assert_eq!(
        (&start["name"], &start["op"], &start["input"]),
        (
            &json!("Start clauth"),
            &json!("setup.turn-on"),
            &json!({"service": "clauth daemon"})
        ),
        "{usage:#}"
    );
    let cli = a.lab.op.ok(&["agents", "accounts", "usage"]);
    assert!(
        cli.contains(
            "Claude Code: use unknown. Start clauth: legionctl setup turn-on 'clauth daemon'"
        ),
        "{cli}"
    );
    let seed = a.lab.op.ok(&["views", "render", "accounts", "--json"]);
    assert!(seed.contains("Start clauth"), "the row's button: {seed}");
    // its step: the CLI line as it is printed starts clauth's daemon (its user unit), and the use is read
    let started = a.lab.op.ok(&["setup", "turn-on", "clauth daemon"]);
    assert!(
        started.contains("clauth daemon: started (legionctl-clauth.service)"),
        "{started}"
    );
    assert_eq!(
        row(&a.read(&[]), "changed", "claude")["text"],
        "Claude Code: clauth has not read the use yet."
    );
    // Claude Code's sessions as clauth lists them: the time in local time (not clauth's UTC text), and no account
    // column (clauth names none for a session it did not start)
    let at = "2026-10-17T21:58:30+00:00";
    write(
        &home.join("clauth-sessions.json"),
        &json!([{"id": "s1", "last_ran_profile": null, "workspace": "/data/git/agency", "updated": at,
                 "first_message": "hi", "last_message": null, "tokens": null, "cost": null}])
        .to_string(),
    );
    let local = std::process::Command::new("date")
        .args(["-d", at, "+%Y-%m-%d %H:%M"])
        .output()
        .unwrap();
    let local = String::from_utf8(local.stdout).unwrap().trim().to_string();
    let seed = a.lab.op.ok(&["views", "render", "accounts"]);
    let sessions = seed
        .split("Claude Code sessions")
        .nth(1)
        .unwrap_or_else(|| panic!("{seed}"));
    assert!(
        sessions.contains(&local)
            && sessions.contains("/data/git/agency")
            && !sessions.contains(at),
        "{local}: {sessions}"
    );
    assert!(!sessions.contains("Account"), "{sessions}");
}

/// A status feed of another schema is one error that names both versions.
fn another_schema_is_one_error(a: &AccountsLab) {
    let home = a.lab.home.to_path_buf();
    let mut other = serde_json::from_str::<Value>(
        &std::fs::read_to_string(home.join("clauth-status.json")).unwrap(),
    )
    .unwrap();
    other["schema"] = json!(3);
    other["clauth_version"] = json!("0.18.0");
    std::fs::write(home.join("clauth-status.json"), other.to_string()).unwrap();
    let refused = a.lab.op.cli_in(&home, &["agents", "accounts", "usage"]);
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains(
            "clauth 0.18.0 writes its status in schema 3; legionctl reads schema 2 (clauth 0.17)"
        ),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
}

/// todo-188, without clauth: the Accounts view shows the current logins and why there is no use, `setup check` has
/// clauth as not installed, and `setup apply` installs it with cargo (`~0.17`, locked), starts its daemon as the
/// user unit and gives Claude Code clauth's MCP server; then the use is clauth's. A clauth of another version is one
/// plain doctor finding with no action (Set up does not replace the user's clauth).
#[test]
fn without_clauth_the_current_logins_show_and_setup_installs_and_starts_it() {
    let lab = Lab::new("no-clauth", &shipped_plugin());
    lab.stand_in_ctx();
    let home = lab.home.to_path_buf();
    write(
        &home.join(".claude/.credentials.json"),
        &json!({"claudeAiOauth": {"accessToken": "tok-live", "refreshToken": "r-live"}})
            .to_string(),
    );
    let usage: Value =
        serde_json::from_str(&lab.op.ok(&["agents", "accounts", "usage", "--json"])).unwrap();
    let current = row(&usage, "current", "claude");
    assert_eq!(
        (&current["text"], &current["actions"][0]["name"]),
        (
            &json!("Claude Code, the current login: use unknown."),
            &json!("Install clauth")
        )
    );
    let item = |what: &str| -> String {
        let check: Value = serde_json::from_str(&lab.op.ok(&["setup", "check", "--json"])).unwrap();
        check["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["what"] == what)
            .map_or_else(String::new, |i| i["status"].as_str().unwrap().to_string())
    };
    assert_eq!(item("clauth"), "not installed");
    let out = lab.op.cli_in(&home, &["setup", "apply"]);
    let done = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for line in [
        "clauth: installed (~0.17) with cargo",
        "clauth daemon: started (legionctl-clauth.service)",
        "clauth mcp: added to Claude Code",
    ] {
        assert!(done.contains(line), "{line}: {done}");
    }
    let log = |name: &str| std::fs::read_to_string(home.join(name)).unwrap_or_default();
    assert_eq!(
        log("cargo.log"),
        "install clauth --version ~0.17 --locked\n"
    );
    assert!(
        log("systemctl.log").contains("--user enable --now legionctl-clauth.service\n"),
        "{}",
        log("systemctl.log")
    );
    assert!(
        log("claude.log").contains(&format!(
            "mcp add --scope user clauth -- {} mcp",
            home.join(".cargo/bin/clauth").display()
        )),
        "{}",
        log("claude.log")
    );
    assert_eq!(
        [item("clauth"), item("clauth daemon"), item("clauth mcp")],
        ["current", "current", "current"]
    );
    clauth::status(&home, &[]);
    let usage: Value =
        serde_json::from_str(&lab.op.ok(&["agents", "accounts", "usage", "--json"])).unwrap();
    assert_eq!(
        row(&usage, "current", "claude")["text"],
        "Claude Code, the current login: use unknown."
    );

    std::fs::write(home.join("clauth-version"), "0.18.0").unwrap();
    let doctor: Value = serde_json::from_str(&lab.op.ok(&["doctor", "--json"])).unwrap();
    let found: Vec<&Value> = doctor["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["text"].as_str().unwrap().starts_with("setup: clauth"))
        .collect();
    assert_eq!(found.len(), 1, "{doctor:#}");
    assert_eq!(
        found[0]["text"],
        "setup: clauth version 0.18.0: legionctl reads clauth 0.17; the accounts may read wrong until clauth 0.17 is \
         installed"
    );
    assert_eq!(found[0]["actions"], json!([]));
}

/// todo-179 (5), r324: the current logins and clauth's profiles are accounts, in a Scratch HOME laid out as clauth
/// 0.17.0 leaves it (main and alt, alt active, `~/.claude/.credentials.json` a link to alt's file, main holding the
/// same login) and a Codex login that no clauth profile holds. Claude Code's owner is the one `clauth which` names;
/// main says it holds the same login as alt (no read); the Codex login says clauth does not read it. Capture never
/// copies a login. r340: when Claude Code's refresh replaced the link with a plain file of a new token (`clauth which`:
/// unknown), the live login is still alt's (clauth's active profile).
#[test]
fn the_current_logins_and_clauths_profiles_are_accounts_and_capture_never_copies_a_login() {
    use clauth::profile;
    let a = AccountsLab::new("current");
    let home = a.lab.home.to_path_buf();
    let clauth = |name: &str, token: &str| {
        let path = home.join(format!(".clauth/profiles/{name}/credentials.json"));
        write(
            &path,
            &json!({"claudeAiOauth": {"accessToken": format!("tok-{token}"), "refreshToken": format!("r-{token}")}})
                .to_string(),
        );
        path
    };
    write(
        &home.join(".clauth/profiles.toml"),
        "active_profile = \"alt\"\nprofiles = [\"main\", \"alt\"]\n",
    );
    clauth("main", "alt");
    let alt = clauth("alt", "alt");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::os::unix::fs::symlink(&alt, home.join(".claude/.credentials.json")).unwrap();
    write(&home.join("clauth-which"), "alt\n");
    let codex = home.join(".codex/auth.json");
    write(
        &codex,
        r#"{"tokens": {"access_token": "tok-livex", "refresh_token": "r-livex", "account_id": "acct-9"}}"#,
    );
    clauth::status(
        &home,
        &[
            profile("alt", "claude", true, &[("5h", 21.0), ("7d", 27.0)], None),
            profile("main", "claude", false, &[("5h", 21.0), ("7d", 27.0)], None),
        ],
    );
    let disk = || {
        [
            &alt,
            &codex,
            &home.join(".clauth/profiles/main/credentials.json"),
        ]
        .map(|p| std::fs::read_to_string(p).unwrap())
    };
    let before = disk();

    let usage = a.read(&[]);
    let claude: Vec<[&Value; 3]> = usage
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["agent"] == "claude")
        .map(|r| [&r["account"], &r["source"], &r["active"]])
        .collect();
    assert_eq!(
        claude,
        [
            [&json!("alt"), &json!("current"), &json!(true)],
            [&json!("main"), &json!("clauth"), &json!(false)]
        ],
        "{usage:#}"
    );
    assert_eq!(
        row(&usage, "alt", "claude")["text"],
        "Claude Code, the current login: 5h 21% used, 7d 27% used. Read just now."
    );
    assert_eq!(
        row(&usage, "main", "claude")["text"],
        "Claude Code: main holds the same login as alt: a refresh through one logs the other out. Log in to main again \
         in clauth, or delete one of the two."
    );
    assert_eq!(
        row(&usage, "current", "codex")["text"],
        "Codex, the current login: use unknown."
    );
    let seed = a.lab.op.ok(&["views", "render", "accounts"]);
    assert_eq!(
        seed.matches("holds the same login").count(),
        1,
        "one warning:\n{seed}"
    );

    // r340: Claude Code's refresh replaced clauth's link with a plain file of a new token; clauth which: unknown
    std::fs::remove_file(home.join(".claude/.credentials.json")).unwrap();
    write(
        &home.join(".claude/.credentials.json"),
        &json!({"claudeAiOauth": {"accessToken": "tok-new", "refreshToken": "r-new"}}).to_string(),
    );
    std::fs::remove_file(home.join("clauth-which")).unwrap();
    let usage = a.read(&[]);
    assert_eq!(
        [
            &row(&usage, "alt", "claude")["source"],
            &row(&usage, "main", "claude")["source"]
        ],
        [&json!("current"), &json!("clauth")],
        "{usage:#}"
    );
    capture_never_copies_a_login(&a, &disk, &before);
}

/// `agents accounts capture` in the layout of the test above: no copy, no write but clauth's own capture.
fn capture_never_copies_a_login(
    a: &AccountsLab,
    disk: &dyn Fn() -> [String; 3],
    before: &[String; 3],
) {
    let home = a.lab.home.to_path_buf();
    let kept = a.lab.op.ok(&["agents", "accounts", "capture", "keep"]);
    assert!(
        kept.contains("Claude Code: the current login is clauth account alt already")
            && kept.contains("Codex: the current login is not kept")
            && kept.contains("log in to Codex as a new account instead"),
        "{kept}"
    );
    assert_eq!(&disk(), before, "no login file changed");
    assert!(!home.join("clauth.log").exists(), "clauth was not called");
    // with no active profile, a Claude Code login that no clauth profile holds goes to clauth's capture
    write(
        &home.join(".clauth/profiles.toml"),
        "profiles = [\"main\", \"alt\"]\n",
    );
    let saved = a.lab.op.ok(&["agents", "accounts", "capture", "fresh"]);
    assert!(
        saved.contains("clauth keeps the current login as account fresh"),
        "{saved}"
    );
    assert_eq!(
        std::fs::read_to_string(home.join("clauth.log")).unwrap(),
        "capture fresh stdin=\n"
    );
    std::fs::remove_file(home.join(".claude/.credentials.json")).unwrap();
    let none = a
        .lab
        .op
        .cli_in(&home, &["agents", "accounts", "capture", "none"]);
    assert!(
        !none.status.success()
            && String::from_utf8_lossy(&none.stderr)
                .contains("no login is kept: Codex: the current login is not kept"),
        "{}",
        String::from_utf8_lossy(&none.stderr)
    );
}

/// clauth is the only store of the logins (user decision 2026-10-17). legionctl's old account folder is not read and
/// not moved in silence: doctor says once which profiles it holds, with a Log in action for each, and the folder stays
/// as it was. Log in and switch go to clauth (a stand-in that logs its calls and its stdin: no terminal, so clauth's
/// own browser door is the way in); a switch to an account that clauth does not have names the accounts and calls
/// nothing; a refusal of clauth is the error with clauth's words.
#[test]
fn the_old_account_folder_is_one_doctor_finding_and_logins_go_through_clauth() {
    let u = AccountsLab::new("old-folder");
    let home = u.lab.home.to_path_buf();
    let old = home.join(".local/share/legionctl/accounts");
    write(&old.join("work/claude/.credentials.json"), "old-work-login");
    std::fs::create_dir_all(old.join("spare/codex")).unwrap();
    let doctor: Value = serde_json::from_str(&u.lab.op.ok(&["doctor", "--json"])).unwrap();
    let found: Vec<&Value> = doctor["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["text"].as_str().unwrap().contains("accounts"))
        .collect();
    assert_eq!(found.len(), 1, "{doctor:#}");
    assert_eq!(
        found[0]["text"],
        format!(
            "{} holds legionctl's old account profiles spare, work, not used: clauth keeps the logins now. Log in to \
             each of them in clauth, then delete the folder",
            old.display()
        )
    );
    let actions: Vec<[&Value; 3]> = found[0]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| [&a["name"], &a["op"], &a["input"]])
        .collect();
    assert_eq!(
        actions,
        [
            [
                &json!("Log in to spare"),
                &json!("agents.accounts.login"),
                &json!({"name": "spare"})
            ],
            [
                &json!("Log in to work"),
                &json!("agents.accounts.login"),
                &json!({"name": "work"})
            ],
        ]
    );
    assert_eq!(
        std::fs::read_to_string(old.join("work/claude/.credentials.json")).unwrap(),
        "old-work-login",
        "nothing moved"
    );
    assert!(!doctor.to_string().contains("old-work-login"));

    write(&home.join("clauth-refuse.switch.spare"), "spare is in use");
    let said = u.lab.op.ok(&["agents", "accounts", "login", "work"]);
    assert_eq!(
        said.trim(),
        "clauth keeps the Claude Code login of account work"
    );
    u.lab
        .op
        .ok(&["agents", "accounts", "login", "work", "--codex"]);
    let ghost = u
        .lab
        .op
        .cli_in(&home, &["agents", "accounts", "switch", "ghost"]);
    assert!(
        String::from_utf8_lossy(&ghost.stderr)
            .contains("no account ghost; the accounts are: work, spare, old, changed"),
        "{}",
        String::from_utf8_lossy(&ghost.stderr)
    );
    assert_eq!(
        u.lab
            .op
            .ok(&["agents", "accounts", "switch", "work"])
            .trim(),
        "account work is the current login"
    );
    let refused = u
        .lab
        .op
        .cli_in(&home, &["agents", "accounts", "switch", "spare"]);
    assert!(
        !refused.status.success()
            && String::from_utf8_lossy(&refused.stderr)
                .contains("clauth switch spare: spare is in use"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(home.join("clauth.log")).unwrap(),
        "login work stdin=\nlogin work --codex --browser stdin=\nswitch work stdin=\n"
    );
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

/// This process's `PATH` without the folders in the user's home.
fn system_path() -> OsString {
    let home = std::env::var_os("HOME").unwrap();
    std::env::join_paths(
        std::env::split_paths(&std::env::var_os("PATH").unwrap()).filter(|p| !p.starts_with(&home)),
    )
    .unwrap()
}

/// The legionctl rows of Codex in `setup check` (not its agent files): (what, status).
fn codex_rows(lab: &Lab) -> Vec<(String, String)> {
    let check: Value = serde_json::from_str(&lab.op.ok(&["setup", "check", "--json"])).unwrap();
    check["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["agent"] == "codex")
        .map(|i| (i["what"].as_str().unwrap(), i["status"].as_str().unwrap()))
        .filter(|(what, _)| what.starts_with("legionctl") && !what.starts_with("legionctl agent"))
        .map(|(what, status)| (what.to_string(), status.to_string()))
        .collect()
}

/// What Codex reads from the plugin it installed at `installed`: the MCP server with the operator's folder, the hooks
/// and the skills.
fn assert_codex_reads_the_plugin(installed: &Path) {
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(installed.join(".codex-plugin/plugin.json")).unwrap(),
    )
    .unwrap();
    let read = |key: &str| -> Value {
        let path = manifest[key].as_str().unwrap().trim_start_matches("./");
        serde_json::from_str(&std::fs::read_to_string(installed.join(path)).unwrap()).unwrap()
    };
    assert_eq!(
        read("mcpServers")["mcpServers"]["legionctl"],
        json!({"command": "legionctl", "args": ["mcp"], "env_vars": ["XDG_RUNTIME_DIR", "LEGIONCTL_SESSION", "LEGIONCTL_TOKEN"]})
    );
    assert_eq!(
        read("hooks")["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        "legionctl hook session-start"
    );
    assert!(
        installed
            .join(manifest["skills"].as_str().unwrap())
            .join("ste100-style/SKILL.md")
            .is_file()
    );
}

/// Codex gets legionctl as a plugin from the package's folder, as Claude Code does (todo-205): `setup apply` on a
/// home with an earlier setup's `[mcp_servers.legionctl]` (no `XDG_RUNTIME_DIR`: Codex starts MCP servers with a cleared
/// environment, so legionctl's server did not find the operator) and its skill links installs the plugin through
/// `codex plugin` (a stand-in with codex 0.163's behaviour), removes the old entry (the user's other settings stay,
/// with a backup) and the old links. The installed plugin gives Codex the MCP server with `XDG_RUNTIME_DIR`, the shared
/// hooks and the shared skills. `setup check` shows the plugin, the server and the hooks for Codex, and the hooks wait
/// for the user's trust in Codex until the user gives it; `skills` shows each skill in Codex's plugin. Codex's copy of
/// an older build is not current, and apply installs it again.
#[test]
fn setup_apply_gives_codex_the_legionctl_plugin_with_its_mcp_server_hooks_and_skills() {
    let lab = Lab::new("codex-plugin", &shipped_plugin());
    lab.stand_in_ctx();
    let config = lab.home.join(".codex/config.toml");
    write(
        &config,
        "model = \"gpt-6.1-sol\"\n\n# added by `legionctl setup`\n[mcp_servers.legionctl]\ncommand = \"legionctl\"\nargs = [\"mcp\"]\n",
    );
    let link = lab.home.join(".codex/skills/legionctl-ste100-style");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(shipped_plugin().join("skills/ste100-style"), &link).unwrap();
    let rows = || codex_rows(&lab);
    let row = |what: &str, status: &str| (what.to_string(), status.to_string());
    assert_eq!(
        rows(),
        [
            row("legionctl plugin", "missing"),
            row(
                "legionctl mcp",
                "outdated: an entry in config.toml replaces the plugin's"
            ),
            row("legionctl hooks", "missing"),
        ]
    );

    let out = lab.op.cli_in(&lab.home, &["setup", "apply"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let log = std::fs::read_to_string(lab.home.join("codex.log")).unwrap();
    let market = shipped_plugin().parent().unwrap().to_path_buf();
    assert!(
        log.contains(&format!("plugin marketplace add {}", market.display()))
            && log.contains("plugin add legionctl@lenovolegionlinux"),
        "{log}"
    );
    let now: toml::Table = std::fs::read_to_string(&config).unwrap().parse().unwrap();
    assert_eq!(
        now["model"].as_str(),
        Some("gpt-6.1-sol"),
        "the user's settings stay"
    );
    assert!(
        now.get("mcp_servers")
            .and_then(|s| s.get("legionctl"))
            .is_none(),
        "{now:#?}"
    );
    assert!(config.with_extension("toml.legionctl-bak").is_file());
    assert!(!link.is_symlink(), "the old skill link is gone");

    let installed = lab
        .home
        .join(".codex/plugins/cache/lenovolegionlinux/legionctl/0.6.0");
    assert_codex_reads_the_plugin(&installed);
    let waiting = legionctl_core::plugin::CODEX_HOOKS_UNTRUSTED;
    assert_eq!(
        rows(),
        [
            row("legionctl plugin", "current"),
            row("legionctl mcp", "current"),
            row("legionctl hooks", waiting),
        ]
    );
    // the user trusts the hooks in Codex (Codex writes its trust; here the stand-in's list)
    let keys: Vec<String> = [
        "session_start",
        "user_prompt_submit",
        "pre_tool_use",
        "subagent_start",
        "post_tool_use",
        "pre_compact",
        "stop",
        "session_end",
        "subagent_stop",
    ]
    .iter()
    .map(|e| format!("legionctl@lenovolegionlinux:hooks/hooks.json:{e}:0:0"))
    .collect();
    // a hook that this build adds (subagent_stop) waits for its own trust
    let earlier = &keys[..keys.len() - 1];
    write(&lab.home.join("codex-trusted"), &earlier.join("\n"));
    assert_eq!(rows()[2], row("legionctl hooks", waiting));
    write(&lab.home.join("codex-trusted"), &keys.join("\n"));
    assert_eq!(rows()[2], row("legionctl hooks", "current"));
    let skills: Value = serde_json::from_str(&lab.op.ok(&["skills", "--json"])).unwrap();
    let ste = skills
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "ste100-style")
        .unwrap_or_else(|| panic!("{skills:#}"));
    assert_eq!(ste["codex"], json!(installed.join("skills/ste100-style")));

    // Codex keeps a copy: after an update of the package its copy is of another build, and apply installs it again
    write(&installed.join("hooks/hooks.json"), "{}\n");
    assert_eq!(rows()[0], row("legionctl plugin", "not of this build"));
    let out = lab.op.cli_in(&lab.home, &["setup", "apply"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(rows()[0], row("legionctl plugin", "current"));
}

/// The `sandbox access` rows of `setup check`: (agent, status, path).
fn sandbox_rows(lab: &Lab) -> Vec<(String, String, String)> {
    let check: Value = serde_json::from_str(&lab.op.ok(&["setup", "check", "--json"])).unwrap();
    check["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["what"] == "sandbox access")
        .map(|i| {
            let [agent, status, path] =
                ["agent", "status", "path"].map(|k| i[k].as_str().unwrap().to_string());
            (agent, status, path)
        })
        .collect()
}

/// A Codex rules file of the user's own at legionctl's name, or legionctl's with rules the user added: the row says so,
/// doctor gives no fix, apply stops before it writes, and the file stays as it was.
fn a_users_own_codex_rules_file_stays(lab: &Lab, rules: &Path) {
    let added = "prefix_rule(pattern = [\"git\", \"push\"], decision = \"forbidden\")\n";
    let ours = std::fs::read_to_string(rules).unwrap();
    for own in [added.to_string(), format!("{ours}{added}")] {
        a_users_rules_file_stays(lab, rules, &own);
    }
}

fn a_users_rules_file_stays(lab: &Lab, rules: &Path, own: &str) {
    write(rules, own);
    let said = format!(
        "{} holds rules of your own: move them to another .rules file, then set up again",
        rules.display()
    );
    assert_eq!(sandbox_rows(lab)[1].1, said);
    let doctor = String::from_utf8_lossy(&lab.op.cli(&["doctor"]).stdout).into_owned();
    assert!(
        doctor.contains(&said) && !doctor.contains(&format!("{said} (fix:")),
        "{doctor}"
    );
    let out = lab.op.cli_in(&lab.home, &["setup", "apply"]);
    assert!(
        !out.status.success() && String::from_utf8_lossy(&out.stderr).contains(&said),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(rules).unwrap(), own);
    std::fs::remove_file(rules).unwrap();
}

/// The one backup that legionctl made of the settings file `settings`, as JSON.
fn only_backup(settings: &Path) -> Value {
    let backups: Vec<_> = std::fs::read_dir(settings.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().contains("settings.json.legionctl-"))
        .collect();
    assert_eq!(backups.len(), 1, "{backups:?}");
    serde_json::from_str(&std::fs::read_to_string(&backups[0]).unwrap()).unwrap()
}

/// The folders of a lab's user-level entries (AOS-9), after [`user_level_folders`] made its repository and configs.
struct Folders {
    /// dossier.root and workspace.root, resolved.
    roots: [String; 2],
    /// The folders with write access: the roots, legionctl's data, state and cache.
    written: [String; 5],
    /// legionctl's config folder, to read.
    read: String,
    /// The repository's `.git`.
    git_dir: String,
    /// The user's Codex config.
    codex_config: PathBuf,
}

/// A repository with a legionctl workspace whose Codex config names its own profile `repo`; the user's Codex config
/// with the custom default profile `mine` (one entry of the user's, read), an entry of the user's in `repo` and the
/// repository's `.git` write that r471-r473 put there; and the rule file that legionctl wrote before AOS-9.
fn user_level_folders(lab: &Lab, rules: &Path) -> Folders {
    // a repository with a legionctl workspace, whose Codex config names its own profile
    let commit = [
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "--allow-empty",
        "-qm",
        "init",
    ];
    let made = Command::new("git")
        .args(commit)
        .current_dir(&lab.repo)
        .status();
    assert!(made.unwrap().success());
    write(
        &lab.repo.join(".codex/config.toml"),
        "default_permissions = \"repo\"\n",
    );
    let session = [("LEGIONCTL_SESSION", "s-sandbox")];
    for args in [&["dossier", "use", "port"][..], &["workspace", "open"]] {
        let out = lab.op.run(&lab.repo, &session, args, "");
        assert!(out.status.success(), "{args:?}: {out:?}");
    }
    let config: Value = serde_json::from_str(&lab.op.ok(&["config", "show", "--json"])).unwrap();
    let roots = [
        &config["value"]["dossier"]["root"],
        &config["value"]["workspace"]["root"],
    ]
    .map(|r| {
        r.as_str()
            .unwrap()
            .replace("~/", &format!("{}/", lab.home.display()))
    });
    let written = [
        roots[0].clone(),
        roots[1].clone(),
        lab.home
            .join(".local/share/legionctl")
            .display()
            .to_string(),
        lab.home
            .join(".local/state/legionctl")
            .display()
            .to_string(),
        lab.home.join(".cache/legionctl").display().to_string(),
    ];
    let read = lab.home.join(".config/legionctl").display().to_string();
    let git_dir = lab.repo.join(".git").display().to_string();
    let codex_config = lab.home.join(".codex/config.toml");
    let own_codex = format!(
        "default_permissions = \"mine\"\n\n[permissions.mine.filesystem]\n\"{}\" = \"read\"\n\n\
         [permissions.repo.filesystem]\n\"/run/user/1000/ssh-agent.socket\" = \"deny\"\n\"{git_dir}\" = \"write\"\n",
        roots[0]
    );
    write(&codex_config, &own_codex);
    // the rule file that legionctl wrote before AOS-9
    write(
        rules,
        "# Written by `legionctl setup apply`: Codex asks you before it runs legionctl outside its sandbox\n\
         prefix_rule(pattern = [\"legionctl\"], decision = \"prompt\")\n",
    );
    Folders {
        roots,
        written,
        read,
        git_dir,
        codex_config,
    }
}

impl Folders {
    /// The user's Claude Code settings `own` with legionctl's entries after them, and none of its workspace git and rm
    /// rules (no repository opts in).
    fn claude_settings(&self, own: &Value) -> Value {
        let (written, read) = (&self.written, &self.read);
        let mut merged = own.clone();
        merged["sandbox"]["excludedCommands"] = json!(["git status *", "legionctl *"]);
        let mut allow_read = vec![json!("~/.cargo"), json!("~/.gitconfig")];
        allow_read.extend(written.iter().map(|f| json!(f)));
        allow_read.push(json!(read));
        merged["sandbox"]["filesystem"]["allowRead"] = json!(allow_read);
        merged["sandbox"]["filesystem"]["allowWrite"] = json!(written);
        merged["permissions"]["allow"] = json!([
            "Bash(ls *)",
            "Bash(legionctl dossier *)",
            "Bash(legionctl status *)",
        ]);
        merged
    }

    /// The folders in the user's profile `mine` and in the repository's profile `repo`, each with the repository's
    /// .git when it opts in (`git`); the user's own entries stay.
    fn assert_codex_profiles(&self, git: bool) {
        let (roots, written, read, git_dir) =
            (&self.roots, &self.written, &self.read, &self.git_dir);
        let codex: toml::Table = std::fs::read_to_string(&self.codex_config)
            .unwrap()
            .parse()
            .unwrap();
        let filesystem = |profile: &str| -> Vec<(String, String)> {
            codex["permissions"][profile]["filesystem"]
                .as_table()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
                .collect()
        };
        let entries = |first: (&str, &str)| -> Vec<(String, String)> {
            let mut all: Vec<(String, String)> = std::iter::once(first)
                .chain(written[1..].iter().map(|f| (f.as_str(), "write")))
                .chain([(read.as_str(), "read")])
                .chain(git.then_some((git_dir.as_str(), "write")))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            all.sort();
            all
        };
        assert_eq!(
            filesystem("mine"),
            entries((&roots[0], "read")),
            "the user's entry stays"
        );
        let mut repo_profile = entries((&roots[0], "write"));
        repo_profile.push(("/run/user/1000/ssh-agent.socket".into(), "deny".into()));
        repo_profile.sort();
        assert_eq!(filesystem("repo"), repo_profile);
        assert_eq!(codex["default_permissions"].as_str(), Some("mine"));
    }
}

/// legionctl dossier and status run with no prompt; Codex asks the user first for each other command of the client, as
/// `legionctl --help` lists them.
fn assert_codex_rules_cover_the_client(lab: &Lab, rules: &Path) {
    // dossier and status run with no prompt; Codex asks the user first for each other command of the client
    let help = lab.op.ok(&["--help"]);
    let mut commands: Vec<String> = help
        .lines()
        .skip_while(|l| !l.starts_with("Commands:"))
        .skip(1)
        .take_while(|l| l.starts_with("  "))
        .map(|l| l.split_whitespace().next().unwrap().to_string())
        .filter(|c| c != "dossier" && c != "status")
        .chain(["-C".into(), "--json".into()])
        .collect();
    commands.sort();
    let quoted: Vec<String> = commands.iter().map(|c| format!("\"{c}\"")).collect();
    assert_eq!(
        std::fs::read_to_string(rules).unwrap(),
        format!(
            "# Written by `legionctl setup apply`: Codex runs legionctl dossier and status outside its sandbox, \
             and asks you before each other legionctl command\n\
             prefix_rule(pattern = [\"legionctl\", [\"dossier\", \"status\"]], decision = \"allow\")\n\
             prefix_rule(pattern = [\"legionctl\", [{}]], decision = \"prompt\")\n",
            quoted.join(", ")
        )
    );
}

/// A host agent with its sandbox on reaches legionctl (plan#todo-227 AOS-5): Claude Code's Bash sandbox blocks the
/// operator's socket on Linux and agency-os's project settings block ~/.gitconfig; Codex's sandbox denies every
/// connect when its network is off. `setup apply` runs `legionctl` outside both sandboxes (Claude Code's
/// sandbox.excludedCommands, a Codex rule) and lets git read ~/.gitconfig (sandbox.filesystem.allowRead), keeping the
/// user's own entries; check and doctor show each row until then. A settings file that is not JSON is never
/// overwritten. AOS-9 (feedbacks/0011, plan#todo-229): the user-level entries that agency-os committed in its own
/// settings come from setup, from the resolved settings: legionctl dossier and status with no prompt (Claude Code
/// permissions.allow; a Codex allow rule, and a prompt rule for every other command of `legionctl --help`, because the
/// strictest Codex rule wins), and legionctl's folders open to both sandboxes; Codex gets them in the user's custom
/// profile and in the profile that a repository with workspaces names (Codex merges the user's entries into it); an
/// entry that the user set stays. The rule file of r466 is legionctl's: setup replaces it. feedbacks/0014: the git and
/// rm rules in all workspaces and the `.git` write that r471-r473 wrote go away (no repository opted in).
#[test]
fn setup_apply_lets_sandboxed_agents_run_legionctl_and_keeps_the_users_own_sandbox_entries() {
    let lab = Lab::new("sandbox", &shipped_plugin());
    lab.stand_in_ctx();
    let settings = lab.home.join(".claude/settings.json");
    let rules = lab.home.join(".codex/rules/legionctl.rules");
    let folders = user_level_folders(&lab, &rules);
    let own = json!({
        "sandbox": {
            "enabled": true,
            "excludedCommands": ["git status *"],
            "filesystem": {"denyRead": ["~/.*"], "allowRead": ["~/.cargo"]}
        },
        "permissions": {"allow": ["Bash(ls *)", format!("Bash(git -C {}/*)", folders.roots[1]),
            format!("Bash(rm -rf {}/*)", folders.roots[1])]}
    });
    write(&settings, &serde_json::to_string_pretty(&own).unwrap());
    let rows = || sandbox_rows(&lab);
    let want = |claude: &str, codex: &str| {
        vec![
            (
                "claude-code".to_string(),
                claude.to_string(),
                settings.display().to_string(),
            ),
            ("codex".into(), codex.into(), rules.display().to_string()),
        ]
    };
    assert_eq!(rows(), want("missing", "not of this build"));
    let doctor = String::from_utf8_lossy(&lab.op.cli(&["doctor"]).stdout).into_owned();
    assert!(
        doctor.contains(
            "setup: sandbox access for claude-code is missing (fix: legionctl setup apply)"
        ) && doctor.contains(
            "setup: sandbox access for codex is not of this build (fix: legionctl setup apply)"
        ),
        "{doctor}"
    );

    let apply = || {
        let out = lab.op.cli_in(&lab.home, &["setup", "apply"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let done = apply();
    for said in [
        format!(
            "sandbox access: legionctl runs outside Claude Code's sandbox, its folders and workspaces are open to it; git \
             and rm run there with no prompt only for a repository that opts in ({})",
            settings.display()
        ),
        format!(
            "sandbox access: legionctl runs outside Codex's sandbox ({})",
            rules.display()
        ),
        format!(
            "sandbox access: legionctl's folders and workspaces are open to Codex's sandbox; it writes the .git only \
             of a repository that opts in ({})",
            folders.codex_config.display()
        ),
    ] {
        assert!(done.contains(&said), "{said}\n{done}");
    }
    let merged = folders.claude_settings(&own);
    assert_eq!(
        serde_json::from_str::<Value>(&std::fs::read_to_string(&settings).unwrap()).unwrap(),
        merged
    );
    assert_codex_rules_cover_the_client(&lab, &rules);
    folders.assert_codex_profiles(false);
    // one write, so one backup: the file as the user had it
    assert_eq!(only_backup(&settings), own);
    assert_eq!(rows(), want("current", "current"));
    assert!(!apply().contains("sandbox access"), "written once");
    a_users_own_codex_rules_file_stays(&lab, &rules);

    // settings that are not JSON: the row names the file, apply fails and changes nothing
    write(&settings, "{ \"sandbox\": ");
    let row = rows()[0].1.clone();
    assert!(
        row.starts_with(&format!(
            "{} is not valid JSON; fix it before setup: EOF while parsing",
            settings.display()
        )),
        "{row}"
    );
    let doctor = String::from_utf8_lossy(&lab.op.cli(&["doctor"]).stdout).into_owned();
    assert!(
        doctor.contains("is not valid JSON; fix it before setup")
            && !doctor
                .contains("before setup: EOF while parsing a value at line 1 column 13 (fix:"),
        "a file apply cannot mend gets no fix: {doctor}"
    );
    let out = lab.op.cli_in(&lab.home, &["setup", "apply"]);
    assert!(
        !out.status.success()
            && String::from_utf8_lossy(&out.stderr)
                .contains("is not valid JSON; fix it before setup"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&settings).unwrap(),
        "{ \"sandbox\": "
    );
}

/// feedbacks/0014: Claude Code's git and rm rules in a repository's workspaces and Codex's write of its `.git` come only
/// from the user's own layer for that repository (`~/.config/legionctl/repos/<repo>`): a repository's `.legionctl`,
/// allowed or not, cannot grant them to itself; turned on, setup writes rules for this repository's workspaces only
/// (Claude's `*` matches every subcommand, `git push` too) and the `.git` write in both Codex profiles; turned off, the
/// rows are outdated, doctor gives the fix and setup takes them out again.
#[test]
fn the_git_and_rm_rules_and_codex_git_write_are_opt_in_per_repository() {
    let lab = Lab::new("sandbox-opt-in", &shipped_plugin());
    lab.stand_in_ctx();
    let settings = lab.home.join(".claude/settings.json");
    let folders = user_level_folders(&lab, &lab.home.join(".codex/rules/legionctl.rules"));
    let own = json!({"permissions": {"allow": ["Bash(ls *)"]}});
    write(&settings, &own.to_string());
    let apply = || {
        let out = lab.op.cli_in(&lab.home, &["setup", "apply"]);
        assert!(out.status.success(), "{out:?}");
    };
    let states = || -> Vec<String> { sandbox_rows(&lab).into_iter().map(|r| r.1).collect() };
    apply();
    let both = r#"{"workspace": {"claude_allows_git_rm": true, "codex_writes_git": true}}"#;
    let project = lab.repo.join(".legionctl/config.jsonc");
    write(&project, both);
    trust::allow(&lab.home, &project);
    let shown = lab.op.cli_in(&lab.repo, &["config", "show", "--json"]);
    let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert!(
        shown["problems"].to_string().contains(&format!(
            "{}: workspace.claude_allows_git_rm is set only in your own layer for one repository \
             (~/.config/legionctl/repos/<repo>); ignored",
            project.display()
        )),
        "{shown:#}"
    );
    assert_eq!(
        states(),
        ["current", "current"],
        "a repository grants itself nothing"
    );
    std::fs::remove_file(&project).unwrap();

    // only `--scope user-repo` sets them, the way a person does
    let set = |key: &str, value: &str, scope: &str| {
        let out = lab
            .op
            .cli_in(&lab.repo, &["config", "set", key, value, "--scope", scope]);
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    for scope in ["user", "project"] {
        let (done, said) = set("workspace.claude_allows_git_rm", "true", scope);
        assert!(!done && said.contains("is set only for"), "{scope}: {said}");
    }
    for key in [
        "workspace.claude_allows_git_rm",
        "workspace.codex_writes_git",
    ] {
        let (done, said) = set(key, "true", "user-repo");
        assert!(done, "{key}: {said}");
    }
    assert_eq!(states(), ["missing", "missing"]);
    apply();
    let allow = |settings: &Path| -> Vec<String> {
        let now: Value = serde_json::from_str(&std::fs::read_to_string(settings).unwrap()).unwrap();
        now["permissions"]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap().into())
            .collect()
    };
    let (root, name) = (
        &folders.roots[1],
        lab.repo.file_name().unwrap().to_string_lossy(),
    );
    let rules: Vec<String> = ["git -C", "rm", "rm -r", "rm -rf", "rmdir"]
        .iter()
        .map(|command| format!("Bash({command} {root}/{name}/*)"))
        .collect();
    assert_eq!(
        allow(&settings)[3..],
        rules[..],
        "this repository's workspaces only"
    );
    folders.assert_codex_profiles(true);
    assert_eq!(states(), ["current", "current"]);

    for key in [
        "workspace.claude_allows_git_rm",
        "workspace.codex_writes_git",
    ] {
        let (done, said) = set(key, "false", "user-repo");
        assert!(done, "{key}: {said}");
    }
    assert_eq!(states(), ["outdated", "outdated"]);
    let doctor = String::from_utf8_lossy(&lab.op.cli(&["doctor"]).stdout).into_owned();
    assert!(
        doctor.contains(
            "setup: sandbox access for claude-code is outdated (fix: legionctl setup apply)"
        ),
        "{doctor}"
    );
    apply();
    assert_eq!(
        json!(allow(&settings)),
        folders.claude_settings(&own)["permissions"]["allow"]
    );
    folders.assert_codex_profiles(false);
    assert_eq!(states(), ["current", "current"]);
}
