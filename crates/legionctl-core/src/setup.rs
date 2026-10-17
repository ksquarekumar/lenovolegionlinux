//! Agent setup: make ctx (<https://ctx.rs>) and legionctl available to Claude Code and Codex out of the box.
//!
//! - ctx: its bundled skill and MCP server for both agents (`ctx integrations install skill|mcp`).
//! - Codex: legionctl's MCP server in ~/.codex/config.toml, the plugin's skills linked into ~/.codex/skills, and each
//!   agent config as a Codex agent in ~/.codex/agents/<name>.toml (instructions, model and effort only).
//! - Claude Code gets legionctl's MCP server, hooks and skills from the plugin itself.
//! - The systemd user manager (and so the operator and its checks) gets ~/.local/bin, ~/.cargo/bin and bun's global
//!   folder (`bun pm -g bin` with `BUN_INSTALL=~/.bun`) before its PATH through ~/.config/environment.d/99-legionctl.conf; it reads the file
//!   at the next login.
//! - The ACP adapters of `session.acp` (headless runs, the app's chat bar), which bun runs
//!   (`bun --bun x --no-install <adapter>`): a missing one is installed with `bun add -g` from `session.acp_packages`.
//!
//! - Claude Code: the legionctl plugin from the package's folder (`claude plugin marketplace add`, `install`).
//! - clauth (the account engine, a runtime dependency): installed with `cargo install clauth --version ~0.17
//!   --locked` when missing, its daemon started as the user unit `legionctl-clauth.service` (it reads each account's
//!   use and switches at the limit), and its MCP server in Claude Code (`claude mcp add --scope user clauth`). A
//!   clauth outside the supported versions is reported, not replaced.
//! - GNOME Shell: the OSD extension enabled for the user.
//! - systemd: legionctl-system and the user's legionctl-operator.socket run (the package turns them on; the user can
//!   turn them off). A start of legionctl-system asks polkit.
//!
//! `check()` only reports; `apply()` changes user-level files, starts the services and returns what it did.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};
use legionctl_machine::system;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::archetypes::{Sources, written_by_legionctl};
use crate::config::SessionConfig;

/// ctx names for the agents legionctl sets up.
pub const AGENTS: [&str; 2] = ["claude-code", "codex"];
const PLUGIN: &str = "/usr/share/legionctl/agent-plugin";
/// The step that turns the machine daemon on, for a message that says it is off.
pub const SYSTEM_ON: &str =
    "turn it on with `sudo systemctl enable --now legionctl-system.service`";
/// legionctl's services: the row's name, the `systemctl` arguments before the verb, and the unit.
const SERVICES: [(&str, &[&str], &str); 2] = [
    ("legionctl-system", &[], "legionctl-system.service"),
    (
        "legionctl-operator",
        &["--user"],
        "legionctl-operator.socket",
    ),
];

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Item {
    /// e.g. "ctx skill", "ctx mcp", "legionctl mcp", "legionctl skill ste100"
    pub what: String,
    pub agent: String,
    /// "current" when in place; otherwise what is wrong (missing, outdated, `invalid_config`, ...)
    pub status: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Report {
    pub ctx_version: Option<String>,
    pub items: Vec<Item>,
}

impl Report {
    #[must_use]
    pub fn needs_attention(&self) -> Vec<&Item> {
        self.items
            .iter()
            .filter(|i| i.status != "current")
            .collect()
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// A tool that is usually not on the PATH of non-interactive shells (hooks, MCP servers, the operator): its install
/// dir under HOME first, then PATH.
pub(crate) fn tool_bin(name: &str, install_dir: &str) -> Option<PathBuf> {
    let local = home().join(install_dir).join(name);
    if local.is_file() {
        return Some(local);
    }
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file())
}

/// bun (<https://bun.sh>): its installer puts it in ~/.bun/bin.
fn bun_bin() -> Option<PathBuf> {
    tool_bin("bun", ".bun/bin")
}

/// `bun` with one global folder, `~/.bun` (`BUN_INSTALL`, bun's default): without it bun takes
/// `$XDG_CACHE_HOME/.bun`, and the user's shell and the operator differ in that variable.
fn bun_global(bun: &Path) -> Command {
    let mut command = Command::new(bun);
    command.env("BUN_INSTALL", home().join(".bun"));
    command
}

/// The folder of bun's global commands (`bun pm -g bin`), when bun is installed and has global packages.
///
/// # Errors
/// bun is installed and `bun pm -g bin` fails for another reason.
fn bun_global_bin() -> Result<Option<PathBuf>> {
    let Some(bun) = bun_bin() else {
        return Ok(None);
    };
    let out = bun_global(&bun)
        .args(["pm", "-g", "bin"])
        .output()
        .with_context(|| format!("running {} pm -g bin", bun.display()))?;
    // bun 1.x before its first `bun add -g`: the global folder has no package.json, so no global command exists
    if String::from_utf8_lossy(&out.stderr).contains("No package.json was found") {
        return Ok(None);
    }
    ensure!(
        out.status.success(),
        "bun pm -g bin: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(Some(PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim(),
    )))
}

pub(crate) fn ctx_bin() -> Option<PathBuf> {
    tool_bin("ctx", ".local/bin")
}

/// tokei (`cargo install tokei`): LOC of planned units.
pub(crate) fn tokei_bin() -> Option<PathBuf> {
    tool_bin("tokei", ".cargo/bin")
}

fn ctx(args: &[&str]) -> Result<String> {
    let bin =
        ctx_bin().context("ctx is not installed; run `legionctl setup apply --install-ctx`")?;
    let out = Command::new(bin)
        .args(args)
        .output()
        .context("running ctx")?;
    ensure!(
        out.status.success(),
        "ctx {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Parse `ctx integrations status <kind> --format json` into items.
///
/// # Errors
/// The output is not JSON with a `results` list of `agent`, `status` and `path` (a string or null); the error holds
/// the output.
fn parse_ctx_status(kind: &str, json: &str) -> Result<Vec<Item>> {
    let parse = || -> Result<Vec<Item>> {
        let value: serde_json::Value = serde_json::from_str(json)?;
        let field = |r: &serde_json::Value, key: &str| -> Result<String> {
            r[key]
                .as_str()
                .map(Into::into)
                .with_context(|| format!("no {key} in {r}"))
        };
        value["results"]
            .as_array()
            .context("no results list")?
            .iter()
            .map(|r| {
                Ok(Item {
                    what: format!("ctx {kind}"),
                    agent: field(r, "agent")?,
                    status: field(r, "status")?,
                    // null: ctx knows no path for that agent
                    path: match &r["path"] {
                        serde_json::Value::Null => String::new(),
                        _ => field(r, "path")?,
                    },
                })
            })
            .collect()
    };
    parse().with_context(|| format!("ctx integrations status {kind} gave: {}", json.trim()))
}

fn ctx_items() -> Vec<Item> {
    let mut items = Vec::new();
    for kind in ["skill", "mcp"] {
        let mut args = vec!["integrations", "status", kind, "--format", "json"];
        for agent in AGENTS {
            args.extend(["--agent", agent]);
        }
        match ctx(&args).and_then(|json| parse_ctx_status(kind, &json)) {
            Ok(found) => items.extend(found),
            Err(e) => items.push(Item {
                what: format!("ctx {kind}"),
                agent: "all".into(),
                status: format!("{e:#}"),
                path: String::new(),
            }),
        }
    }
    items
}

fn codex_config() -> PathBuf {
    home().join(".codex/config.toml")
}

/// # Errors
/// `text` is not valid TOML.
pub fn codex_has_legionctl(text: &str) -> Result<bool> {
    let table: toml::Table = text
        .parse()
        .context("~/.codex/config.toml is not valid TOML; fix it before setup")?;
    Ok(table
        .get("mcp_servers")
        .and_then(|s| s.get("legionctl"))
        .and_then(|e| e.get("command"))
        .and_then(|c| c.as_str())
        == Some("legionctl"))
}

/// The agent plugin's folder (`LEGIONCTL_PLUGIN` for a checkout's `integrations/agent-plugin`).
#[must_use]
pub fn plugin_root() -> PathBuf {
    std::env::var_os("LEGIONCTL_PLUGIN").map_or_else(|| PathBuf::from(PLUGIN), PathBuf::from)
}

#[must_use]
pub fn plugin_skills() -> PathBuf {
    plugin_root().join("skills")
}

fn codex_agents_dir() -> PathBuf {
    home().join(".codex/agents")
}

/// One item per agent config for Codex: current when its file has the text that `apply` writes.
fn codex_agent_items(agents: &Sources) -> Vec<Item> {
    let files = match agents.codex_agents() {
        Ok(files) => files,
        Err(e) => {
            return vec![Item {
                what: "legionctl agents".into(),
                agent: "codex".into(),
                status: format!("{e:#}"),
                path: codex_agents_dir().display().to_string(),
            }];
        }
    };
    files
        .into_iter()
        .map(|(name, text)| {
            let path = codex_agents_dir().join(format!("{name}.toml"));
            let status = match std::fs::read_to_string(&path) {
                Ok(have) if have == text => "current",
                Ok(_) => "outdated",
                Err(_) => "missing",
            };
            Item {
                what: format!("legionctl agent {name}"),
                agent: "codex".into(),
                status: status.into(),
                path: path.display().to_string(),
            }
        })
        .collect()
}

fn skill_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(plugin_skills())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("SKILL.md").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn codex_items() -> Vec<Item> {
    let config = codex_config();
    let status = match legionctl_machine::sysroot::read_file(&config) {
        Ok(Some(text)) => match codex_has_legionctl(&text) {
            Ok(true) => "current".into(),
            Ok(false) => "missing".into(),
            Err(e) => e.to_string(),
        },
        Ok(None) => "missing".into(),
        Err(e) => format!("{e:#}"),
    };
    let mut items = vec![Item {
        what: "legionctl mcp".into(),
        agent: "codex".into(),
        status,
        path: config.display().to_string(),
    }];
    for name in skill_names() {
        let link = home()
            .join(".codex/skills")
            .join(format!("legionctl-{name}"));
        let status = if std::fs::read_link(&link).is_ok_and(|t| t == plugin_skills().join(&name)) {
            "current"
        } else {
            "missing"
        };
        items.push(Item {
            what: format!("legionctl skill {name}"),
            agent: "codex".into(),
            status: status.into(),
            path: link.display().to_string(),
        });
    }
    items
}

/// 99-: environment.d(5) reads the files of all its folders in file-name order, so a system file (Ubuntu's
/// 90-*.conf) that sets PATH after ours would replace it; the last file's `$PATH` keeps all earlier values.
fn path_conf() -> PathBuf {
    home().join(".config/environment.d/99-legionctl.conf")
}

/// The folders that `path_conf()` puts before the user manager's PATH.
fn path_folders() -> Result<Vec<PathBuf>> {
    let home = home();
    let mut folders = vec![home.join(".local/bin"), home.join(".cargo/bin")];
    folders.extend(bun_global_bin()?);
    Ok(folders)
}

/// The text of `path_conf()`: absolute folders, because environment.d(5) expands only variables (`$PATH`), not `~`.
fn path_conf_text(folders: &[PathBuf]) -> String {
    let folders: Vec<String> = folders.iter().map(|f| f.display().to_string()).collect();
    format!(
        "# Written by `legionctl setup apply`: the user's tool folders for the systemd user manager (the operator's checks)\n\
         PATH={}:$PATH\n",
        folders.join(":")
    )
}

fn path_item(folders: &[PathBuf]) -> Item {
    let status = match std::fs::read_to_string(path_conf()) {
        Ok(have) if have == path_conf_text(folders) => "current",
        Ok(_) => "outdated",
        Err(_) => "missing",
    };
    Item {
        what: "legionctl PATH".into(),
        agent: "systemd".into(),
        status: status.into(),
        path: path_conf().display().to_string(),
    }
}

/// The adapter of ACP command `argv`: the package word after bun's `x` (`bun --bun x --no-install <adapter>`), else
/// the command itself.
#[must_use]
pub fn adapter_program(argv: &[String]) -> Option<&str> {
    match argv.first() {
        Some(bun) if Path::new(bun).file_name() == Some("bun".as_ref()) => argv
            .iter()
            .skip_while(|a| *a != "x")
            .skip(1)
            .find(|a| !a.starts_with('-'))
            .map(String::as_str),
        first => first.map(String::as_str),
    }
}

/// Where the operator's headless sessions on the host find their programs: the folders of `path_conf()`, then this
/// process's PATH. The user manager reads `path_conf()` only at its start, so the operator's own PATH can lack them.
///
/// # Errors
/// bun is installed and `bun pm -g bin` fails.
pub fn session_path() -> Result<Vec<PathBuf>> {
    let mut search = path_folders()?;
    search.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    Ok(search)
}

/// Whether the adapter at `path` comes from npm package `package` (bun links a command into the package's folder);
/// true with no package.
fn of_package(path: &Path, package: Option<&str>) -> bool {
    package.is_none_or(|p| {
        path.canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .contains(&format!("/node_modules/{p}/"))
    })
}

/// `program` in `search`, or the file itself for a path.
fn find(program: &str, search: &[PathBuf]) -> Option<PathBuf> {
    if program.contains('/') {
        Some(PathBuf::from(program)).filter(|p| p.is_file())
    } else {
        search.iter().map(|d| d.join(program)).find(|p| p.is_file())
    }
}

/// An install of an ACP adapter failed: trying again can help (the network, the npm registry).
#[derive(Debug)]
pub struct InstallFailed(pub String);

impl std::fmt::Display for InstallFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InstallFailed {}

/// Install ACP adapter `adapter` from npm package `package` with `bun add -g` into bun's global folder.
///
/// # Errors
/// bun is not installed, or the install fails or gives no `adapter` ([`InstallFailed`]).
fn add_adapter(adapter: &str, package: &str) -> Result<()> {
    let bun = bun_bin().context(
        "Install bun (the bun package): legionctl installs and runs the agent's adapter with it",
    )?;
    let out = bun_global(&bun)
        .args(["add", "-g", package])
        .output()
        .with_context(|| format!("running {} add -g {package}", bun.display()))?;
    if !out.status.success() {
        return Err(InstallFailed(format!(
            "Installing the adapter {adapter} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
        .into());
    }
    if !find(adapter, &session_path()?).is_some_and(|path| of_package(&path, Some(package))) {
        return Err(InstallFailed(format!(
            "Installing the adapter {adapter} failed: bun add -g {package} gave no {adapter}"
        ))
        .into());
    }
    Ok(())
}

/// Make ACP command `argv` ready to start with the programs in `search`: a missing adapter with a package in
/// `session.acp_packages` is installed first, after `announce` got its name.
///
/// # Errors
/// Its program (bun) is not installed, the adapter has no package, or its install fails ([`InstallFailed`]).
pub fn ready(
    session: &SessionConfig,
    argv: &[String],
    search: &[PathBuf],
    announce: &mut dyn FnMut(&str),
) -> Result<()> {
    let program = argv.first().context("the agent's ACP command is empty")?;
    let adapter = adapter_program(argv).unwrap_or(program);
    ensure!(
        adapter == program || find(program, search).is_some(),
        "Install {program} (the {program} package): the agent's adapter starts with it"
    );
    let package = session.acp_packages.get(adapter);
    // an adapter of another package (r324's Zed codex-acp) is replaced by the configured one
    if find(adapter, search).is_some_and(|path| of_package(&path, package.map(String::as_str))) {
        return Ok(());
    }
    let package = package.with_context(|| {
        format!("Install the agent's adapter {adapter}, or name its npm package in config session.acp_packages")
    })?;
    announce(adapter);
    add_adapter(adapter, package)
}

/// One item per ACP adapter of `session.acp` ([`adapter_program`]): current when it is in `search`
/// ([`session_path`]) and comes from its package (`session.acp_packages`: bun links a command into the package's
/// folder); else missing, or from another package (the Zed codex-acp before the ACP project's, which gives the same
/// command), with the package as the path.
fn acp_items(session: &SessionConfig, search: &[PathBuf]) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    for (agent, argv) in &session.acp {
        let Some(command) = adapter_program(argv) else {
            continue;
        };
        let what = format!("ACP adapter {command}");
        if let Some(item) = items.iter_mut().find(|i| i.what == what) {
            item.agent = format!("{},{agent}", item.agent);
            continue;
        }
        let package = session.acp_packages.get(command);
        let found = find(command, search);
        let (status, path) = match found {
            Some(path) if !of_package(&path, package.map(String::as_str)) => (
                "from another package".to_string(),
                package.cloned().unwrap_or_default(),
            ),
            Some(path) => ("current".to_string(), path.display().to_string()),
            None => match package {
                Some(package) => ("missing".to_string(), package.clone()),
                None => (
                    "missing (no package in session.acp_packages)".to_string(),
                    String::new(),
                ),
            },
        };
        items.push(Item {
            what,
            agent: agent.clone(),
            status,
            path,
        });
    }
    items
}

/// What is in place and what is not.
///
/// # Errors
/// bun is installed and `bun pm -g bin` fails.
pub fn check(agents: &Sources, session: &SessionConfig) -> Result<Report> {
    let ctx_version = ctx(&["--version"]).ok().map(|v| v.trim().to_string());
    let mut items = if ctx_version.is_some() {
        ctx_items()
    } else {
        vec![Item {
            what: "ctx".into(),
            agent: "all".into(),
            status: "not installed".into(),
            path: String::new(),
        }]
    };
    items.extend(codex_items());
    items.extend(codex_agent_items(agents));
    let folders = path_folders()?;
    items.push(path_item(&folders));
    items.extend(acp_items(session, &session_path()?));
    items.extend(claude_plugin_item());
    items.extend(osd_item());
    items.extend(SERVICES.map(service_item));
    items.extend(clauth_items());
    Ok(Report { ctx_version, items })
}

/// Install ctx with its signed installer (user-level, ~/.local/bin), analytics off, shell rc files untouched.
fn install_ctx() -> Result<()> {
    let script = "curl -fsSL https://ctx.rs/install | CTX_ANALYTICS_ENABLED=false CTX_INSTALL_NO_MODIFY_PATH=1 sh";
    let out = Command::new("sh")
        .args(["-c", script])
        .output()
        .context("running the ctx installer")?;
    ensure!(
        out.status.success(),
        "ctx install failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

/// Bring every item to "current". Returns one line per change.
///
/// # Errors
/// An agent config that a Codex subagent cannot apply (before any change), a config file, skill link or agent file
/// cannot be read, parsed or written, ctx cannot be installed or run, or a missing ACP adapter has no package, no bun
/// to install it, or is not there after `bun add -g`.
pub fn apply(
    install_missing_ctx: bool,
    agents: &Sources,
    session: &SessionConfig,
) -> Result<Vec<String>> {
    let codex_agents = agents.codex_agents()?;
    // the services first: the other steps can stop at a tool that is missing
    let mut done = turn_on_services()?;
    if ctx_bin().is_none() {
        ensure!(
            install_missing_ctx,
            "ctx is not installed; rerun with --install-ctx to install it"
        );
        install_ctx()?;
        done.push("installed ctx".to_string());
    }
    for kind in ["skill", "mcp"] {
        let mut args = vec!["integrations", "install", kind, "--quiet"];
        for agent in AGENTS {
            args.extend(["--agent", agent]);
        }
        ctx(&args)?;
        done.push(format!("ctx {kind}: installed for {}", AGENTS.join(", ")));
    }

    let config = codex_config();
    let text = legionctl_machine::sysroot::read_file(&config)?.unwrap_or_default();
    if !codex_has_legionctl(&text)? {
        ensure!(
            !text.contains("[mcp_servers.legionctl]"),
            "{} has [mcp_servers.legionctl] with another command; set `command = \"legionctl\"` in it",
            config.display()
        );
        if config.exists() {
            std::fs::copy(&config, config.with_extension("toml.legionctl-bak"))
                .context("backing up codex config")?;
        }
        std::fs::create_dir_all(config.parent().context("codex config has no parent")?)?;
        let block = "\n# added by `legionctl setup`\n[mcp_servers.legionctl]\ncommand = \"legionctl\"\nargs = [\"mcp\"]\n";
        let updated = format!("{}{block}", text.trim_end_matches('\n'));
        ensure!(
            codex_has_legionctl(&updated)?,
            "refusing to write: result would not contain [mcp_servers.legionctl]"
        );
        std::fs::write(&config, updated)?;
        done.push(format!(
            "legionctl mcp: added to {} (backup *.legionctl-bak)",
            config.display()
        ));
    }

    let skills_dir = home().join(".codex/skills");
    // skills that were renamed or removed leave dangling legionctl-* links behind; drop only those
    for entry in std::fs::read_dir(&skills_dir)
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("legionctl-")
            && path.is_symlink()
            && !path.exists()
        {
            std::fs::remove_file(&path)?;
            done.push(format!("removed stale link {}", path.display()));
        }
    }
    for name in skill_names() {
        let target = plugin_skills().join(&name);
        let link = skills_dir.join(format!("legionctl-{name}"));
        if std::fs::read_link(&link).is_ok_and(|t| t == target) {
            continue;
        }
        std::fs::create_dir_all(&skills_dir)?;
        if link.is_symlink() {
            std::fs::remove_file(&link)?;
        }
        ensure!(
            !link.exists(),
            "{} exists and is not a legionctl link; not replacing it",
            link.display()
        );
        std::os::unix::fs::symlink(&target, &link)?;
        done.push(format!("legionctl skill {name}: linked for codex"));
    }

    done.extend(write_codex_agents(&codex_agents)?);
    done.extend(install_acp_adapters(session)?);
    done.extend(install_claude_plugin()?);
    done.extend(enable_osd()?);
    done.extend(set_up_clauth()?);
    let folders = path_folders()?;
    if path_item(&folders).status != "current" {
        let conf = path_conf();
        std::fs::create_dir_all(conf.parent().context("environment.d file has no parent")?)?;
        std::fs::write(&conf, path_conf_text(&folders))?;
        done.push(format!(
            "legionctl PATH: written to {} (active at the next login)",
            conf.display()
        ));
    }
    Ok(done)
}

/// The user unit that runs clauth's daemon.
const CLAUTH_UNIT: &str = "legionctl-clauth.service";

/// clauth's version (`clauth 0.17.0`), when it is installed.
fn clauth_version() -> Result<Option<String>> {
    Ok(crate::accounts::clauth(&["--version"])?
        .map(|v| v.split_whitespace().nth(1).unwrap_or_default().to_string()))
}

/// clauth, its daemon, and its MCP server in Claude Code.
fn clauth_items() -> Vec<Item> {
    let supported = crate::accounts::CLAUTH_SUPPORTED;
    let (status, version) = match clauth_version() {
        Ok(None) => ("not installed".to_string(), None),
        Ok(Some(v))
            if v.strip_prefix(supported)
                .is_some_and(|r| r.starts_with('.')) =>
        {
            ("current".into(), Some(v))
        }
        Ok(Some(v)) => (
            format!("version {v}: legionctl reads clauth {supported}"),
            Some(v),
        ),
        Err(e) => (format!("cannot read: {e:#}"), None),
    };
    let mut items = vec![Item {
        what: "clauth".into(),
        agent: "all".into(),
        status,
        path: crate::accounts::clauth_bin()
            .map(|b| b.display().to_string())
            .unwrap_or_default(),
    }];
    if version.is_none() {
        return items;
    }
    items.push(Item {
        what: "clauth daemon".into(),
        agent: "all".into(),
        status: if crate::accounts::daemon_runs() {
            "current".into()
        } else {
            "not running".into()
        },
        path: CLAUTH_UNIT.into(),
    });
    if claude_bin().is_some() {
        items.push(Item {
            what: "clauth mcp".into(),
            agent: "claude-code".into(),
            status: if claude_has_clauth_mcp() {
                "current".into()
            } else {
                "missing".into()
            },
            path: home().join(".claude.json").display().to_string(),
        });
    }
    items
}

/// Claude Code has clauth's MCP server: in `~/.claude.json` or through clauth's own plugin.
fn claude_has_clauth_mcp() -> bool {
    let has = |path: PathBuf, pick: &dyn Fn(&serde_json::Value) -> bool| {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .is_some_and(|v| pick(&v))
    };
    has(home().join(".claude.json"), &|v| {
        v["mcpServers"]["clauth"].is_object()
    }) || has(
        home().join(".claude/plugins/installed_plugins.json"),
        &|v| {
            v["plugins"]
                .as_object()
                .is_some_and(|p| p.keys().any(|k| k.starts_with("clauth@")))
        },
    )
}

/// `<bin> <args>`, which must succeed.
fn run(bin: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new(bin)
        .args(args)
        .output()
        .with_context(|| format!("running {} {}", bin.display(), args.join(" ")))?;
    ensure!(
        out.status.success(),
        "{} {}: {}",
        bin.display(),
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

/// Install clauth when it is missing, start its daemon, and give Claude Code its MCP server.
fn set_up_clauth() -> Result<Vec<String>> {
    let mut done = Vec::new();
    if crate::accounts::clauth_bin().is_none() {
        let cargo = tool_bin("cargo", ".cargo/bin").context(
            "clauth is installed with cargo, and cargo is not installed (the rust package has it)",
        )?;
        let range = format!("~{}", crate::accounts::CLAUTH_SUPPORTED);
        run(
            &cargo,
            &["install", "clauth", "--version", &range, "--locked"],
        )?;
        done.push(format!("clauth: installed ({range}) with cargo"));
    }
    for item in clauth_items().into_iter().filter(|i| i.status != "current") {
        match item.what.as_str() {
            "clauth daemon" => {
                run(&systemctl()?, &["--user", "enable", "--now", CLAUTH_UNIT])?;
                done.push(format!("clauth daemon: started ({CLAUTH_UNIT})"));
            }
            "clauth mcp" => {
                let claude = claude_bin().context("Claude Code is not installed")?;
                let clauth = crate::accounts::clauth_bin().context("clauth is not installed")?;
                let clauth = clauth.to_string_lossy();
                run(
                    &claude,
                    &[
                        "mcp", "add", "--scope", "user", "clauth", "--", &clauth, "mcp",
                    ],
                )?;
                done.push(
                    "clauth mcp: added to Claude Code (active at the next session start)".into(),
                );
            }
            // a clauth of other versions stays: the check reports it
            _ => {}
        }
    }
    Ok(done)
}

/// Claude Code (`claude`): its installer puts it in ~/.local/bin.
fn claude_bin() -> Option<PathBuf> {
    tool_bin("claude", ".local/bin")
}

/// The legionctl plugin of Claude Code, when Claude Code is installed: current when Claude Code has it from the
/// package's folder and its hooks are of this build (doctor's plugin findings say what else is wrong).
fn claude_plugin_item() -> Option<Item> {
    claude_bin()?;
    let home = home();
    let status = match crate::plugin::claude_plugin_dir(&home) {
        None => "missing",
        Some(_) if crate::plugin::findings(&home).is_empty() => "current",
        Some(_) => "not of this build",
    };
    Some(Item {
        what: "legionctl plugin".into(),
        agent: "claude-code".into(),
        status: status.into(),
        path: crate::plugin::package_marketplace().display().to_string(),
    })
}

/// `claude <args>`, which must succeed.
fn claude(claude: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new(claude)
        .args(args)
        .output()
        .with_context(|| format!("running claude {}", args.join(" ")))?;
    ensure!(
        out.status.success(),
        "claude {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

/// Install the legionctl plugin into Claude Code from the package's folder (the marketplace of the same name from
/// another folder goes first), so the plugin and this legionctl are one build.
fn install_claude_plugin() -> Result<Vec<String>> {
    let Some(item) = claude_plugin_item().filter(|i| i.status != "current") else {
        return Ok(Vec::new());
    };
    let bin = claude_bin().context("Claude Code is not installed")?;
    let (market, key) = (crate::plugin::MARKETPLACE, crate::plugin::PLUGIN_KEY);
    if crate::plugin::marketplace_source(&home()).is_some() {
        claude(&bin, &["plugin", "marketplace", "remove", market])?;
    }
    claude(&bin, &["plugin", "marketplace", "add", &item.path])?;
    claude(&bin, &["plugin", "install", key])?;
    Ok(vec![format!(
        "legionctl plugin: installed into Claude Code from {} (active at the next session start)",
        item.path
    )])
}

/// The OSD extension, where GNOME Shell's `gnome-extensions` is (doctor shows the same row).
fn osd_item() -> Option<Item> {
    let bin = tool_bin("gnome-extensions", ".local/bin")?;
    let status = match system::osd_status(&bin) {
        Ok(status) => status?.into(),
        Err(e) => format!("cannot read: {e:#}"),
    };
    Some(Item {
        what: "OSD extension".into(),
        agent: "gnome-shell".into(),
        status,
        path: system::OSD_EXTENSION.into(),
    })
}

/// Enable the OSD extension (it shows at the next login).
fn enable_osd() -> Result<Vec<String>> {
    // the other states are no step of setup: check shows them
    if osd_item().is_none_or(|i| i.status != "not enabled") {
        return Ok(Vec::new());
    }
    let bin =
        tool_bin("gnome-extensions", ".local/bin").context("gnome-extensions is not installed")?;
    let out = Command::new(&bin)
        .args(["enable", system::OSD_EXTENSION])
        .output()
        .context("running gnome-extensions enable")?;
    ensure!(
        out.status.success(),
        "gnome-extensions enable {}: {}",
        system::OSD_EXTENSION,
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(vec![
        "OSD extension: enabled (it shows at the next login)".to_string(),
    ])
}

/// A service of legionctl: current when it runs; else its state (off, failed) and, as its path, the line that turns it
/// on.
fn service_item((what, scope, unit): (&str, &[&str], &str)) -> Item {
    let status = match systemctl().and_then(|bin| {
        Ok(Command::new(bin)
            .args(scope)
            .args(["is-active", unit])
            .output()?)
    }) {
        Ok(out) => match String::from_utf8_lossy(&out.stdout).trim() {
            "active" => "current".into(),
            "inactive" => "off".into(),
            "" => format!(
                "cannot read: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            state => state.into(),
        },
        Err(e) => format!("cannot read: {e:#}"),
    };
    let on = if scope.is_empty() {
        "sudo systemctl"
    } else {
        "systemctl --user"
    };
    Item {
        what: what.into(),
        // the service manager it runs in
        agent: if scope.is_empty() { "machine" } else { "user" }.into(),
        status,
        path: format!("{on} enable --now {unit}"),
    }
}

/// systemctl, from PATH (a test puts a stand-in in ~/.local/bin).
fn systemctl() -> Result<PathBuf> {
    tool_bin("systemctl", ".local/bin").context("systemctl is not installed")
}

/// Enable and start each service that does not run; polkit asks for legionctl-system.
fn turn_on_services() -> Result<Vec<String>> {
    SERVICES
        .into_iter()
        .filter_map(|s| turn_on_one(s).transpose())
        .collect()
}

/// Enable and start the service `what` (a row of [`check`]) when it does not run; polkit asks for legionctl-system.
///
/// # Errors
/// `what` is not a service of legionctl, or systemctl is missing or fails.
pub fn turn_on(what: &str) -> Result<Vec<String>> {
    let service = SERVICES
        .into_iter()
        .find(|s| s.0 == what)
        .with_context(|| {
            format!(
                "{what} is not a service of legionctl; the services are {}",
                SERVICES.map(|s| s.0).join(", ")
            )
        })?;
    Ok(turn_on_one(service)?.into_iter().collect())
}

/// One service: its line when this turned it on, none when it ran.
fn turn_on_one(service @ (what, scope, unit): (&str, &[&str], &str)) -> Result<Option<String>> {
    if service_item(service).status == "current" {
        return Ok(None);
    }
    let out = Command::new(systemctl()?)
        .args(scope)
        .args(["enable", "--now", unit])
        .output()
        .context("running systemctl")?;
    ensure!(
        out.status.success(),
        "{what} did not start: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(Some(format!("{what}: turned on")))
}

/// `bun add -g` for each ACP adapter of `session.acp` that is missing or from another package (bun links the command
/// to the new one); one line per adapter.
fn install_acp_adapters(session: &SessionConfig) -> Result<Vec<String>> {
    let mut done = Vec::new();
    for item in acp_items(session, &session_path()?) {
        if item.status == "current" {
            continue;
        }
        ensure!(
            item.status == "missing" || item.status == "from another package",
            "{} for {}: {}",
            item.what,
            item.agent,
            item.status
        );
        add_adapter(item.what.trim_start_matches("ACP adapter "), &item.path)?;
        done.push(format!(
            "{}: installed {} with bun add -g",
            item.what, item.path
        ));
    }
    Ok(done)
}

/// Write each Codex agent file of `files` (name -> text), and remove each file that legionctl wrote for a config that
/// is gone. A file that legionctl did not write stays.
fn write_codex_agents(files: &std::collections::BTreeMap<String, String>) -> Result<Vec<String>> {
    let mut done = Vec::new();
    let agents_dir = codex_agents_dir();
    // an agent file that legionctl wrote for a config that is gone
    for entry in std::fs::read_dir(&agents_dir)
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        let name = path.file_stem().map(|n| n.to_string_lossy().into_owned());
        if path.extension().is_some_and(|e| e == "toml")
            && name.is_some_and(|n| !files.contains_key(&n))
            && std::fs::read_to_string(&path).is_ok_and(|t| written_by_legionctl(&t))
        {
            std::fs::remove_file(&path)?;
            done.push(format!("removed agent file {}", path.display()));
        }
    }
    for (name, text) in files {
        let path = agents_dir.join(format!("{name}.toml"));
        let have = std::fs::read_to_string(&path).ok();
        if have.as_deref() == Some(text.as_str()) {
            continue;
        }
        ensure!(
            have.as_deref().is_none_or(written_by_legionctl),
            "{} exists and legionctl did not write it; not replacing it",
            path.display()
        );
        std::fs::create_dir_all(&agents_dir)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)?;
        done.push(format!("legionctl agent {name}: written for codex"));
    }
    Ok(done)
}
