//! Config layers (note 14): the built-in defaults, `/etc/legionctl`, `~/.config/legionctl` and `<repo>/.legionctl`,
//! merged per key into one [`Config`]. Only the defaults and `/etc/legionctl` set the layer order and the locked
//! keys, so a repository cannot reorder layers or set a locked key. Every value keeps the files that set it, for
//! `legionctl config explain`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use legionctl_machine::load::Thresholds;
use legionctl_machine::profile::read_jsonc;
use legionctl_machine::sysroot::SysRoot;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::reminders::Event;

const DEFAULTS: &str = include_str!("../../../config/defaults.jsonc");
/// The file in the user layer that legionctl writes; it merges after the user's own files.
pub const MANAGED: &str = "managed.jsonc";
const MANAGED_HEADER: &str = "// Written by legionctl (reminders, hooks). Change it with legionctl, or move keys into your own file\n\
                              // in this folder; this file merges after your files.\n";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub config: Meta,
    pub workspace: WorkspaceConfig,
    pub reminders: BTreeMap<String, ReminderConfig>,
    pub hooks: HooksConfig,
    pub dossier: DossierConfig,
    pub session: SessionConfig,
    pub gates: GatesConfig,
    pub mcp: McpConfig,
    pub packages: BTreeMap<String, PackageConfig>,
    pub gateway: GatewayConfig,
    pub load: Thresholds,
    pub plugins: PluginsConfig,
    pub events: EventsConfig,
    /// Namespace -> its limit in the disposable cache (`tools`, `gateway`).
    pub cache: BTreeMap<String, CacheLimit>,
    pub windows: WindowsConfig,
    pub agents: AgentsConfig,
    /// The repository's checks (design 23, E2): `<repo>/.legionctl/checks.jsonc`.
    pub checks: crate::checks::ChecksConfig,
    pub app: AppConfig,
    /// System One decision backends (todo-138).
    pub rlcd: crate::rlcd::RlcdConfig,
    /// Views and their composer (DESIGN-09 G5).
    pub views: ViewsConfig,
    /// Grants of capability sets (DESIGN-13).
    pub runnables: crate::runnables::RunnablesConfig,
    /// The capability sets of the layers (`<layer>/capabilities/*.jsonc`), filled by [`resolve`].
    #[serde(skip)]
    #[schemars(skip)]
    pub capabilities: Vec<crate::runnables::Loaded>,
}

/// How an intent finds or gets its view (DESIGN-09 G5): a stored view with that intent, then rlcd's pick when it is
/// on, then the ui-composer over ACP.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewsConfig {
    /// The composer's model per agent of `session.acp` (ACP `session/set_model`); an agent with none keeps its own.
    pub composer_models: BTreeMap<String, String>,
    /// rlcd picks one of the stored views for an intent that no stored view has word for word.
    pub rlcd: bool,
    /// rlcd's pick counts from this probability on.
    pub rlcd_min: f64,
}

/// The desktop app (`legionctl-app`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// The chat bar's agent (a `session.acp` name) in a thread that has no `/agent` choice; null: none.
    pub agent: Option<String>,
}

/// Agent configs (design 23, B+2): each one is an archetype file plus the fields that it changes.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentsConfig {
    /// Config name -> its layer over its archetype file.
    pub configs: BTreeMap<String, crate::archetypes::AgentConfig>,
    /// Account usage and the switch at the limit (todo-136).
    pub accounts: crate::accounts::AccountsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WindowsConfig {
    pub vm: WindowsVmConfig,
}

/// The installed Windows in a VM (note 05).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WindowsVmConfig {
    /// Host CPUs the VM runs on (a CPU list); Linux keeps the others.
    pub host_cpus: String,
    /// vCPUs Windows sees.
    pub vcpus: u32,
    pub memory_mib: u64,
    /// Memory that must stay free for Linux when the VM starts (VFIO locks all of the VM's memory).
    pub reserve_mib: u64,
    /// USB devices for Windows, as vendor:product (`lsusb`).
    pub usb: Vec<String>,
    /// Linux folders Windows gets through virtiofs (Windows needs the virtio-win drivers and `WinFsp`); the tag is
    /// the folder's name.
    pub shares: Vec<String>,
    /// Shared memory for Looking Glass (Windows' frames in a Linux window while it has the GPU), MiB; 0 is off. 128
    /// holds 4K.
    pub looking_glass_mib: u64,
    /// UEFI firmware (OVMF) code, and the variables a first start copies.
    pub ovmf_code: String,
    pub ovmf_vars: String,
}

/// The agent plugins a user may install through legionctl (A1).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CacheLimit {
    /// Bytes the namespace may hold; the least recently used entries go first.
    pub max_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EventsConfig {
    /// Days the operator keeps events and gateway usage; it deletes older ones when it starts. 0 keeps everything.
    pub keep_days: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PluginsConfig {
    /// Marketplace name (its own, as in its marketplace.json) -> where it comes from and which plugins.
    pub marketplaces: BTreeMap<String, Marketplace>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Marketplace {
    /// `owner/repo`, a git or https URL, or a path.
    pub source: String,
    /// Plugin names allowed from it; `*` allows all.
    pub allow: Vec<String>,
}

impl PluginsConfig {
    /// Why `plugin@marketplace` may not be installed, if it may not.
    #[must_use]
    pub fn refusal(&self, id: &str) -> Option<String> {
        let Some((plugin, market)) = id
            .split_once('@')
            .filter(|(plugin, _)| crate::dossier::valid_name(plugin))
        else {
            return Some(format!(
                "{id}: write it as <plugin>@<marketplace>, the plugin name a-z, 0-9 and '-'"
            ));
        };
        let Some(m) = self.marketplaces.get(market) else {
            return Some(format!(
                "marketplace {market} is not in plugins.marketplaces"
            ));
        };
        (!m.allow.iter().any(|a| a == "*" || a == plugin)).then(|| {
            format!(
                "plugin {plugin} is not allowed from {market} (plugins.marketplaces.{market}.allow)"
            )
        })
    }
}

/// The model gateway in the operator (note 19).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    /// Port on 127.0.0.1; 0 turns the gateway off.
    pub port: u16,
    /// Account name -> provider, base URL and key.
    pub accounts: BTreeMap<String, GatewayAccount>,
    /// Model -> accounts in order; the first with quota and budget left serves the call. `*` serves any model.
    pub pool: BTreeMap<String, Vec<String>>,
    /// How long an exact-cache entry replays (0: no cache).
    pub cache_ttl_s: u64,
    /// `account/model` -> money per day and per 30 days; the entry leaves the pool when one is used.
    pub budgets: BTreeMap<String, Budget>,
    /// Model -> price, for the cost of each call and for budgets.
    pub prices: BTreeMap<String, Price>,
    /// The semantic cache: off when null.
    pub semantic: Option<SemanticConfig>,
}

/// Semantic replay (`x-legionctl-cache: semantic`): the prompt's embedding from `account` (an `OpenAI`-API account:
/// `/v1/embeddings`, its keyring key or none for a local model), nearest kept answer at or above `threshold`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SemanticConfig {
    pub account: String,
    pub model: String,
    /// Cosine similarity (0-1) at which a kept answer replays.
    pub threshold: f64,
}

impl GatewayConfig {
    /// Every budget names an account and a model with a price: without a price no call has a cost.
    ///
    /// # Errors
    /// A budget that cannot be counted.
    pub fn check(&self) -> Result<()> {
        if let Some(semantic) = &self.semantic {
            ensure!(
                self.accounts
                    .get(&semantic.account)
                    .is_some_and(|a| a.provider == "openai"),
                "gateway.semantic.account: {} is not an openai gateway account (embeddings use /v1/embeddings)",
                semantic.account
            );
            ensure!(
                semantic.threshold > 0.0 && semantic.threshold <= 1.0,
                "gateway.semantic.threshold: between 0 and 1"
            );
        }
        for key in self.budgets.keys() {
            let (account, model) = key
                .split_once('/')
                .with_context(|| format!("gateway.budgets.{key}: write it as account/model"))?;
            ensure!(
                self.accounts.contains_key(account),
                "gateway.budgets.{key}: no gateway account {account}"
            );
            ensure!(
                self.prices.contains_key(model),
                "gateway.budgets.{key}: gateway.prices has no {model}, so its cost is unknown"
            );
        }
        Ok(())
    }
}

/// Money limits of one `account/model`, in USD; rolling windows of 24 hours and 30 days.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub day_usd: Option<f64>,
    pub month_usd: Option<f64>,
}

/// USD per million tokens. Input tokens that the provider reads from or writes to its prompt cache cost
/// `cache_read` and `cache_write`; without them they cost `input` (a budget then counts too much, never too little).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

/// The tokens of one call: input not from the prompt cache, input read from it and written to it, and output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokens {
    pub input: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub output: i64,
}

impl Tokens {
    /// Every input token, cached or not.
    #[must_use]
    pub fn all_input(self) -> i64 {
        self.input + self.cache_read + self.cache_write
    }
}

impl Price {
    #[must_use]
    pub fn cost(self, t: Tokens) -> f64 {
        // a call has far fewer than 2^32 tokens: u32 holds them, and f64 holds u32 exactly
        let n = |t: i64| f64::from(u32::try_from(t).unwrap_or(u32::MAX));
        (n(t.input) * self.input
            + n(t.cache_read) * self.cache_read.unwrap_or(self.input)
            + n(t.cache_write) * self.cache_write.unwrap_or(self.input)
            + n(t.output) * self.output)
            / 1e6
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GatewayAccount {
    /// anthropic or openai: the API it speaks.
    pub provider: String,
    pub base_url: String,
    /// The keyring entry with its API key (`secret-tool lookup legionctl-account <name>`); without one the caller's
    /// own credentials go through (Claude Code and Codex logins).
    #[serde(default)]
    pub keyring: bool,
}

/// A package in the store: fetched once, pinned to `version` and a content hash (`legionctl packages`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackageConfig {
    pub kind: crate::packages::PackageKind,
    /// The npm, `PyPI` or crates.io name, the git URL, or a local file or folder (`~/` is the home).
    pub source: String,
    /// The pin: a version, or a git tag or branch. A local path has none.
    #[serde(default)]
    pub version: Option<String>,
    /// How to run its MCP server; `{dir}` is the package's folder in the store.
    #[serde(default)]
    pub run: Vec<String>,
    /// Its server may use the network; without this it runs with no network.
    #[serde(default)]
    pub network: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    /// MCP servers a session's agent reaches, each through `legionctl mcp-proxy` (the allowlist and the call log).
    pub servers: BTreeMap<String, McpServer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpServer {
    /// The stdio server's command line; empty when `package` or `url` gives the server.
    #[serde(default)]
    pub command: Vec<String>,
    /// A remote server (streamable HTTP): `https://mcp.context7.com/mcp`.
    #[serde(default)]
    pub url: Option<String>,
    /// For a remote server: header -> keyring entry (`secret-tool lookup legionctl-mcp <entry>`), read at session
    /// start. The session's agent can read them too (same user and sandbox): keep high-value keys in the gateway.
    #[serde(default)]
    pub keyring_headers: BTreeMap<String, String>,
    /// A package from the store: its `run` line, from the hash-checked folder, without network unless it allows it.
    #[serde(default)]
    pub package: Option<String>,
    /// Tools the agent may call; empty: every tool the server offers.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Tools whose results may be reused: only tools that change nothing and give the same answer for the same
    /// input. Nothing else is cached.
    #[serde(default)]
    pub cache: BTreeMap<String, CacheRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CacheRule {
    /// How long a result stays valid.
    pub ttl_s: u64,
    /// The tool reads the working copy: the git state (HEAD, status, diff) is part of the key.
    #[serde(default)]
    pub workspace: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionConfig {
    /// Agent name -> command line; `{id}` is the session id, `{settings}` the generated Claude Code settings file and
    /// `{mcp}` the generated MCP config (the proxied servers and legionctl's own).
    pub agents: BTreeMap<String, Vec<String>>,
    /// The agents' own state that they write besides the workspace, the dossier and their commits (`~/` is the
    /// home): never the real files. On the host each folder is an overlay whose writes stay in the session, in a
    /// container or VM the session's own copy; each file is the session's own copy.
    pub writable: Vec<String>,
    /// Caches the agent's tools write (`~/` is the home): on the host an overlay whose writes stay in the session; a
    /// container or VM has its own.
    pub caches: Vec<String>,
    /// Agent name -> the command that continues an ended session (`session resume`); placeholders as in `agents`.
    pub resume: BTreeMap<String, Vec<String>>,
    /// Agent name -> the command of its ACP adapter, for headless runs (`session run`).
    pub acp: BTreeMap<String, Vec<String>>,
    /// The agents of `acp` whose adapter can be bound to given tools (a run with `tools`: no file, shell or web tools
    /// of its own, no settings, plugins or MCP servers but legionctl's with those tools); a bound run of another agent
    /// fails.
    pub acp_bind: Vec<String>,
    /// ACP adapter command -> the npm package that gives it: `setup apply` installs a missing one with `bun add -g`.
    pub acp_packages: BTreeMap<String, String>,
    /// Run sessions in a systemd user scope (`legionctl-session-<id>`); off where there is no user systemd.
    pub scope: bool,
    /// Hidden in the sandbox: sockets and folders that would give a way out (a container or VM with the host's
    /// files). A folder becomes empty, a file or socket `/dev/null`.
    pub hidden: Vec<String>,
    /// Skills from the store, as `<package>` or `<package>/<folder in it>` (a folder with `SKILL.md`). When set,
    /// the agents see exactly these skills, read-only, instead of the user's own; when null, the user's own.
    pub skills: Option<Vec<String>>,
    /// What legionctl does by itself with sessions (S4, decided 2026-10-13); off by default.
    pub policy: SessionPolicy,
    /// The agent image of a repository (`workspace pack`) for sessions in a container (W5).
    pub container: ContainerConfig,
    /// The VM disk of a repository (`workspace pack --as vm`) for sessions in a VM (W5).
    pub vm: VmConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VmConfig {
    /// The cloud image (qcow2) a repository's disk starts from; its folder's `SHA512SUMS` checks the download.
    pub base: String,
    /// The size of a repository's disk.
    pub disk_gib: u32,
    pub memory_mib: u64,
    pub cpus: u32,
    /// The virtiofs daemon, which shares the session's folders with the VM.
    pub virtiofsd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContainerConfig {
    /// The image a repository without its own image (no Dockerfile) starts from.
    pub base: String,
    /// npm packages of the agents and their ACP adapters, installed in the agent layer.
    pub npm: Vec<String>,
    /// A static `legionctl` (musl) for the image: the MCP server, the hook client and the git gate there.
    pub client: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionPolicy {
    /// Freeze an interactive session this many minutes after its agent ended a reply with no prompt since; 0 never.
    /// A frozen session reads nothing from its terminal: `legionctl session unpause <id>` lets it go on.
    pub idle_freeze_minutes: u32,
    /// Stop headless runs while memory is under pressure (the `load.busy` limits) and go on with them when it eases.
    pub offload_headless: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GatesConfig {
    pub git: GitGate,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GitGate {
    /// Refused in a session sandbox: a subcommand and its unsafe flags (`reset --hard`, `clean -f`).
    pub deny: Vec<String>,
}

/// `~/x` as a path under the home folder; other paths as they are.
///
/// # Errors
/// The path starts with `~/` and HOME is not set.
pub fn expand_home(path: &str) -> Result<PathBuf> {
    Ok(match path.strip_prefix("~/") {
        Some(rest) => {
            PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(rest)
        }
        None => PathBuf::from(path),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DossierConfig {
    /// `~/` is the home folder.
    pub root: String,
    /// In a repository where a dossier was used, block each prompt of a session that has no dossier.
    pub gate: bool,
    /// At session start and at each prompt, bind a session in a linked repository to the last dossier used there
    /// when no live session holds it.
    pub auto_bind: bool,
    /// Hook events at which the agent is told to set up a session that has no dossier.
    pub nudge: Vec<Event>,
    /// Replies that change the workspace without a dossier write before the agent is told (info), told at the end of
    /// a reply (warn, at twice this), and stopped at the end of each reply until it writes (block, three times);
    /// 0 turns this off.
    pub nudge_turns: u32,
    pub stale_hours: u64,
    /// A message between agents (`thread send`) longer than this, with no link to a dossier artifact, gets a hint in
    /// the answer: put the details in the dossier and link them; it is sent all the same. 0 turns this off.
    pub message_chars: usize,
    /// Class name -> purpose.
    pub classes: BTreeMap<String, String>,
    /// Classes whose artifacts cannot change once written (as front matter `immutable: true`).
    pub immutable_classes: Vec<String>,
    pub plan_sections: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    /// Layer names, lowest first.
    pub precedence: Vec<String>,
    /// Keys or key prefixes that only the defaults and /etc/legionctl may set.
    pub locked: Vec<String>,
    /// Keys or key prefixes that a repository layer file sets only after the user allowed that file's content once
    /// (trust on first use, O-2): until then, and after a Deny, the file's values of these keys are not used.
    pub approve: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    /// Workspaces live in `<root>/<repo>/<dossier>`.
    pub root: PathBuf,
    /// Tell the agent to open a workspace: at session start when there is none, and before a reply ends when it
    /// changed the main checkout instead.
    pub nudge: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReminderConfig {
    pub text: String,
    pub event: Event,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HooksConfig {
    pub actions: BTreeMap<String, CommandAction>,
    pub bindings: BTreeMap<Event, Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommandAction {
    pub command: String,
    pub timeout: u32,
}

/// One config file and the layer it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Source {
    pub layer: String,
    /// `built in` for the defaults.
    pub file: String,
}

/// A value that a file set for a key.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Setting {
    pub source: Source,
    pub value: Value,
}

/// A layer, where its files are, and which of them exist.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Layer {
    pub name: String,
    pub dir: Option<String>,
    pub files: Vec<String>,
}

/// The merged config of one place (the user's, plus a repository's when one applies).
#[derive(Debug, Clone)]
pub struct Resolved {
    pub config: Config,
    pub value: Value,
    pub layers: Vec<Layer>,
    /// Dotted key -> what each file set, lowest first.
    pub trace: BTreeMap<String, Vec<Setting>>,
    /// Files and keys that were not used, and why.
    pub problems: Vec<String>,
    /// Repository layer files whose `config.approve` keys wait for the user's first answer.
    pub unapproved: Vec<Unapproved>,
    user_dir: PathBuf,
}

/// A repository layer file that sets keys of `config.approve` with content the user did not answer yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unapproved {
    pub file: String,
    /// The hash of the parsed content: an answer holds for this content only.
    pub hash: String,
    pub keys: Vec<String>,
}

/// The user's answers to repository layer files (`~/.local/state/legionctl/approvals.json`): file -> the content
/// hash and whether the user allowed it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Approvals(pub BTreeMap<String, (String, bool)>);

impl Approvals {
    fn path() -> PathBuf {
        crate::db::default_path().with_file_name("approvals.json")
    }

    /// # Errors
    /// The file is there but cannot be read or parsed.
    pub fn read() -> Result<Approvals> {
        match std::fs::read(Self::path()) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("approvals.json"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Approvals::default()),
            Err(e) => Err(e).context("reading approvals.json"),
        }
    }

    /// Record the user's answer for `file` at content `hash`.
    ///
    /// # Errors
    /// The file cannot be read or written.
    pub fn answer(file: &str, hash: &str, allowed: bool) -> Result<()> {
        let mut all = Approvals::read()?;
        all.0.insert(file.to_string(), (hash.to_string(), allowed));
        let path = Self::path();
        std::fs::create_dir_all(path.parent().context("no state folder")?)?;
        let part = path.with_extension("part");
        std::fs::write(&part, serde_json::to_vec_pretty(&all)?)?;
        Ok(std::fs::rename(&part, &path)?)
    }
}

/// The content hash of a parsed layer file: what is merged, so a file that changes between its read and the hash
/// cannot pass with another content's answer.
pub(crate) fn content_hash(value: &Value) -> String {
    use blake2::{Blake2b, Digest, digest::consts::U32};
    format!("{:x}", Blake2b::<U32>::digest(value.to_string()))
}

/// Where the system and user layers are.
#[derive(Debug, Clone)]
pub struct Dirs {
    pub system: PathBuf,
    pub user: PathBuf,
}

impl Dirs {
    /// `/etc/legionctl` under the sysroot, and `$XDG_CONFIG_HOME/legionctl` (default `~/.config/legionctl`).
    #[must_use]
    pub fn from_env() -> Dirs {
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
        Dirs {
            system: SysRoot::from_env().path("/etc/legionctl"),
            user: std::env::var_os("XDG_CONFIG_HOME")
                .map_or_else(|| home.join(".config"), PathBuf::from)
                .join("legionctl"),
        }
    }
}

/// The repository that contains `dir`, if any.
pub(crate) fn repo_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .find(|d| d.join(".git").exists())
        .map(Path::to_path_buf)
}

/// `*.jsonc` files of a layer folder in merge order: by name, and the managed file last; none when it is not there.
fn layer_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        entries => entries
            .and_then(Iterator::collect::<std::io::Result<Vec<_>>>)
            .with_context(|| format!("listing {}", dir.display()))?,
    };
    let mut files: Vec<PathBuf> = entries
        .iter()
        .map(std::fs::DirEntry::path)
        .filter(|p| p.extension().is_some_and(|x| x == "jsonc"))
        .collect();
    files.sort_by_key(|p| (p.file_name().is_some_and(|n| n == MANAGED), p.clone()));
    Ok(files)
}

fn locked(locks: &[String], key: &str) -> bool {
    locks
        .iter()
        .any(|l| key == l || key.starts_with(&format!("{l}.")))
}

/// What one merge pass needs besides the two values.
struct Merge<'a> {
    source: &'a Source,
    /// The defaults and /etc/legionctl may set `config` and locked keys.
    trusted: bool,
    locks: &'a [String],
    /// A repository layer file: the `config.approve` keys and the user's answer to this content (none yet: `None`).
    approve: Option<(&'a [String], Option<bool>)>,
    /// The `config.approve` keys this file set that wait for an answer.
    held: Vec<String>,
    trace: &'a mut BTreeMap<String, Vec<Setting>>,
    problems: &'a mut Vec<String>,
}

impl Merge<'_> {
    fn object(&mut self, into: &mut Map<String, Value>, from: Map<String, Value>, prefix: &str) {
        for (key, value) in from {
            let (key, append) = match key.strip_suffix('+') {
                Some(k) => (k.to_string(), true),
                None => (key, false),
            };
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            if !self.trusted
                && (path == "config" || path.starts_with("config.") || locked(self.locks, &path))
            {
                self.problems.push(format!(
                    "{}: {path} is set only by the defaults and /etc/legionctl (locked); ignored",
                    self.source.file
                ));
                continue;
            }
            if let Some((keys, answer)) = self.approve
                && answer != Some(true)
                && locked(keys, &path)
            {
                self.problems.push(match answer {
                    Some(_) => format!(
                        "{}: {path} is not used: the user denied this content (a changed file asks again)",
                        self.source.file
                    ),
                    None => format!(
                        "{}: {path} is not used until the user allows this file (legionctl asks once)",
                        self.source.file
                    ),
                });
                if answer.is_none() {
                    self.held.push(path);
                }
                continue;
            }
            match (into.get_mut(&key), value) {
                (Some(Value::Object(lower)), Value::Object(higher)) => {
                    self.object(lower, higher, &path);
                }
                (_, Value::Null) => {
                    into.remove(&key);
                    self.note(&path, Value::Null);
                }
                (Some(Value::Array(lower)), Value::Array(items)) if append => {
                    lower.extend(items.clone());
                    self.note(&path, Value::Array(items));
                }
                // a new or replaced object: merge it into an empty one, so the trace has every key below it
                (_, Value::Object(higher)) => {
                    let mut fresh = Map::new();
                    self.object(&mut fresh, higher, &path);
                    into.insert(key, Value::Object(fresh));
                }
                (_, value) => {
                    self.note(&path, value.clone());
                    into.insert(key, value);
                }
            }
        }
    }

    fn note(&mut self, path: &str, value: Value) {
        self.trace
            .entry(path.to_string())
            .or_default()
            .push(Setting {
                source: self.source.clone(),
                value,
            });
    }
}

/// Merge the layer files in order: each one's keys over the lower ones, with the locks, and a repository layer
/// file's `config.approve` keys only when the user allowed its content; the files whose keys wait for an answer.
fn merge_layers(
    ordered: Vec<(Source, bool, Value)>,
    (locks, approve, approvals): (&[String], &[String], &Approvals),
    trace: &mut BTreeMap<String, Vec<Setting>>,
    problems: &mut Vec<String>,
) -> (Map<String, Value>, Vec<Unapproved>) {
    let mut merged = Map::new();
    let mut unapproved = Vec::new();
    for (source, trusted, value) in ordered {
        // only a repository layer file needs the user's answer for its content
        let hash = (source.layer == "repo").then(|| content_hash(&value));
        let answer = hash.as_ref().map(|hash| {
            approvals
                .0
                .get(&source.file)
                .filter(|(h, _)| h == hash)
                .map(|(_, allowed)| *allowed)
        });
        if let Value::Object(map) = value {
            let mut merge = Merge {
                source: &source,
                trusted,
                locks,
                approve: answer.map(|answer| (approve, answer)),
                held: Vec::new(),
                trace,
                problems,
            };
            merge.object(&mut merged, map, "");
            if let (false, Some(hash)) = (merge.held.is_empty(), hash) {
                unapproved.push(Unapproved {
                    file: source.file.clone(),
                    hash,
                    keys: merge.held,
                });
            }
        }
    }
    (merged, unapproved)
}

/// Resolve the config for work in `project` (its repository layer applies), or without a repository.
///
/// # Errors
/// The built-in defaults do not parse, `/etc/legionctl` sets an unknown layer, or the merged config does not match
/// [`Config`] (the message names the key), or a layer folder or file cannot be read or is not a JSONC object (a typo
/// must not drop a setting such as a gate in silence).
pub fn resolve(dirs: &Dirs, project: Option<&Path>) -> Result<Resolved> {
    let defaults: Value =
        serde_json::from_reader(json_comments::StripComments::new(DEFAULTS.as_bytes()))
            .context("parsing the built-in defaults")?;
    let system_dir = dirs.system.clone();
    let user_dir = dirs.user.clone();
    let repo_dir = project.and_then(repo_root).map(|r| r.join(".legionctl"));

    let mut problems = Vec::new();
    let read = |dir: &Path| -> Result<Vec<(PathBuf, Value)>> {
        layer_files(dir)?
            .into_iter()
            .map(|file| match read_jsonc(&file)? {
                Value::Object(map) => Ok((file, Value::Object(map))),
                _ => bail!("{}: not a JSON object", file.display()),
            })
            .collect()
    };
    let system = read(&system_dir)?;
    let user = read(&user_dir)?;
    let repo = match repo_dir.as_deref() {
        Some(dir) => read(dir)?,
        None => Vec::new(),
    };

    // order and locks come only from the defaults and /etc/legionctl
    let trusted_meta = |key: &str| {
        system
            .iter()
            .rev()
            .find_map(|(_, v)| v["config"].get(key).cloned())
            .unwrap_or_else(|| defaults["config"][key].clone())
    };
    let precedence: Vec<String> =
        serde_json::from_value(trusted_meta("precedence")).context("config.precedence")?;
    let locks: Vec<String> =
        serde_json::from_value(trusted_meta("locked")).context("config.locked")?;
    let approve: Vec<String> =
        serde_json::from_value(trusted_meta("approve")).context("config.approve")?;
    // the repository's config files and capability sets need the user's answers
    let approvals = if repo_dir.is_some() {
        Approvals::read()?
    } else {
        Approvals::default()
    };

    let mut layers = Vec::new();
    let mut ordered: Vec<(Source, bool, Value)> = Vec::new();
    for name in &precedence {
        let (dir, files, trusted) = match name.as_str() {
            "defaults" => (
                None,
                vec![(PathBuf::from("built in"), defaults.clone())],
                true,
            ),
            "system" => (Some(&system_dir), system.clone(), true),
            "user" => (Some(&user_dir), user.clone(), false),
            "repo" => (repo_dir.as_ref(), repo.clone(), false),
            other => bail!(
                "config.precedence names an unknown layer {other} (known: defaults, system, user, repo)"
            ),
        };
        layers.push(Layer {
            name: name.clone(),
            dir: dir.map(|d| d.display().to_string()),
            files: files.iter().map(|(f, _)| f.display().to_string()).collect(),
        });
        for (file, value) in files {
            let source = Source {
                layer: name.clone(),
                file: file.display().to_string(),
            };
            ordered.push((source, trusted, value));
        }
    }
    ensure!(
        precedence.first().is_some_and(|l| l == "defaults"),
        "config.precedence must start with defaults"
    );

    let mut trace = BTreeMap::new();
    let (merged, unapproved) = merge_layers(
        ordered,
        (&locks, &approve, &approvals),
        &mut trace,
        &mut problems,
    );
    let value = Value::Object(merged);
    let mut config: Config =
        serde_json::from_value(value.clone()).context("the merged config is not valid")?;
    let mut unapproved = unapproved;
    let mut set_layers = vec![("system", system_dir.clone()), ("user", user_dir.clone())];
    set_layers.extend(repo_dir.iter().map(|d| ("repo", d.clone())));
    config.capabilities =
        crate::runnables::load(&set_layers, &approvals, &mut problems, &mut unapproved)?;
    config.gateway.check()?;
    Ok(Resolved {
        config,
        value,
        layers,
        trace,
        problems,
        unapproved,
        user_dir,
    })
}

/// What `legionctl config explain` shows for one key.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Explanation {
    pub key: String,
    /// The merged value; null when no layer sets it.
    pub value: Value,
    pub locked: bool,
    /// Every file that set this key or a key below it, lowest first.
    pub settings: Vec<(String, Setting)>,
}

impl Resolved {
    /// Whether the built-in defaults set `key`.
    #[must_use]
    pub fn built_in(&self, key: &str) -> bool {
        self.trace
            .iter()
            .filter(|(k, _)| *k == key || k.starts_with(&format!("{key}.")))
            .any(|(_, settings)| settings.iter().any(|s| s.source.layer == "defaults"))
    }

    #[must_use]
    pub fn explain(&self, key: &str) -> Explanation {
        let value = key
            .split('.')
            .try_fold(&self.value, |v, part| v.get(part))
            .cloned()
            .unwrap_or(Value::Null);
        Explanation {
            key: key.to_string(),
            value,
            locked: locked(&self.config.config.locked, key),
            settings: self
                .trace
                .iter()
                .filter(|(k, _)| *k == key || k.starts_with(&format!("{key}.")))
                .flat_map(|(k, settings)| settings.iter().map(move |s| (k.clone(), s.clone())))
                .collect(),
        }
    }

    /// Set `key` (dotted) in the user layer's managed file; `null` removes the key there.
    ///
    /// # Errors
    /// The key is locked, or the managed file cannot be read or written.
    pub fn set_user(&self, key: &str, value: Value) -> Result<()> {
        ensure!(
            !locked(&self.config.config.locked, key) && !key.starts_with("config"),
            "{key} is locked by /etc/legionctl; see `legionctl config explain {key}`"
        );
        let path = self.user_dir.join(MANAGED);
        let mut managed = match read_jsonc(&path) {
            Ok(Value::Object(map)) => map,
            Ok(_) => bail!("{} is not a JSON object", path.display()),
            Err(_) if !path.exists() => Map::new(),
            Err(e) => return Err(e),
        };
        let parts: Vec<&str> = key.split('.').collect();
        let (last, parents) = parts.split_last().context("empty key")?;
        let mut at = &mut managed;
        for part in parents {
            let next = at
                .entry((*part).to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            if !next.is_object() {
                *next = Value::Object(Map::new());
            }
            at = next.as_object_mut().context("object")?;
        }
        at.insert((*last).to_string(), value);
        std::fs::create_dir_all(&self.user_dir)?;
        let tmp = path.with_extension("jsonc.tmp");
        std::fs::write(
            &tmp,
            format!(
                "{MANAGED_HEADER}{}\n",
                serde_json::to_string_pretty(&managed)?
            ),
        )?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}
