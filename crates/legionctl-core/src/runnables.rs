//! Runnables and capability sets (DESIGN-13 M1). A capability set is a named, versioned part of a runnable's manifest
//! (`config/capabilities/<name>.jsonc`, shipped; the same folder name in `/etc/legionctl`, the user's config and a
//! repository's `.legionctl`). A session or member requests sets; the grant is the subset that its environment (where
//! it runs), its parent (the main agent, for a member) and the user layer (`actions.allow`) allow, and a repository
//! set with a powerful part only after the user allowed its content once (trust on first use, O-2). Anything else
//! refuses the start, with the reason. The granted manifest is a fact (`runnable:session:<id>`, attribute `manifest`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::OptionalExtension;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{Approvals, Config, Source, Unapproved};
use crate::session::Inside;

/// The step to operator.db v15: each session's granted connections, where the gateway reads them (JSON of
/// [`Connections`]; none: a session from before grants).
macro_rules! tables {
    () => {
        "
     ALTER TABLE sessions ADD COLUMN connections TEXT;"
    };
}
pub(crate) use tables;

/// The shipped sets. A new set is a new file in `config/capabilities/` and a line here.
const SHIPPED: [&str; 18] = [
    include_str!("../../../config/capabilities/agent-work.jsonc"),
    include_str!("../../../config/capabilities/claude-code.jsonc"),
    include_str!("../../../config/capabilities/code-work.jsonc"),
    include_str!("../../../config/capabilities/codex.jsonc"),
    include_str!("../../../config/capabilities/dossier-write.jsonc"),
    include_str!("../../../config/capabilities/github-read.jsonc"),
    include_str!("../../../config/capabilities/machine-change.jsonc"),
    include_str!("../../../config/capabilities/machine-read.jsonc"),
    include_str!("../../../config/capabilities/mcp-tools.jsonc"),
    include_str!("../../../config/capabilities/node-dev.jsonc"),
    include_str!("../../../config/capabilities/python-dev.jsonc"),
    include_str!("../../../config/capabilities/rust-dev.jsonc"),
    include_str!("../../../config/capabilities/session-own.jsonc"),
    include_str!("../../../config/capabilities/settings-change.jsonc"),
    include_str!("../../../config/capabilities/thread-member.jsonc"),
    include_str!("../../../config/capabilities/views-write.jsonc"),
    include_str!("../../../config/capabilities/web-read.jsonc"),
    include_str!("../../../config/capabilities/workspace-write.jsonc"),
];

/// A runnable's manifest (DESIGN-13): a capability set file, or the granted manifest of a runnable (the union of its
/// granted sets). Each part is optional; a set sets only what it needs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Manifest {
    #[serde(rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// A set: its name, the same as its file's. A granted manifest: the runnable (`session:<id>`).
    pub name: String,
    /// A set: its version; a changed set has a higher one.
    pub version: u32,
    pub description: String,
    /// The code or entrypoint and how it starts.
    pub runnable: Option<Entry>,
    /// Its own `SQLite` for its controlled state: where it lives and where it syncs.
    pub state: Option<State>,
    /// The sandbox kinds it may run in; empty: each.
    pub sandbox: Vec<Inside>,
    pub host: Host,
    /// Known mounts and their mode.
    pub storage: Vec<Storage>,
    /// The ways it may reach the user; empty: none.
    pub channels: Vec<Channel>,
    pub toolverse: Toolverse,
    pub connections: Connections,
    /// The programs it needs.
    pub tools: Vec<Tool>,
    pub budgets: Budgets,
    /// A harness's profile (DESIGN-12): how it runs, what it reaches, and how the session sets it up.
    pub harness: Option<Harness>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Entry {
    /// The command line (a program, a script, an image).
    pub entrypoint: Vec<String>,
    /// A language file's name (`rust`, `node`, `python`).
    pub language: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Place {
    Dossier,
    Repository,
    Workspace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct State {
    /// The database file, relative to where it lives.
    pub sqlite: String,
    pub lives: Place,
    /// Where it syncs to; none: it does not sync.
    #[serde(default)]
    pub syncs_to: Option<Place>,
}

/// The host side (DESIGN-13: the environment enforces it).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Host {
    /// User and system privileges (none is granted yet: a set that names one is refused).
    pub privileges: Vec<String>,
    /// Device nodes (`/dev/...`): bound into a host or container sandbox; a VM gets none.
    pub devices: Vec<String>,
    /// Commands it must not run (`cargo publish`): Claude Code's settings deny them (`Bash(<command>:*)`).
    pub forbidden_commands: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Storage {
    /// An absolute path, or `~/` for the home.
    pub path: String,
    pub mode: Mode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Read,
    /// Writable at its own path.
    Write,
    /// The real folder with every write in the session's layer; on the host only (a container or VM has its own
    /// caches, as `session.caches`).
    Overlay,
    /// Writable at its own path over an overlaid folder above it, so a write reaches the real file.
    WriteThrough,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    /// Notes to the user through main0.
    Notes,
    /// A desktop notice.
    Desktop,
}

/// legionctl's own operations, MCP tools and skills (by class and name).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Toolverse {
    /// Operation classes (`read`, `session`, `workspace`, `machine`).
    pub classes: Vec<String>,
    /// Operation names; `x.*` names a group.
    pub operations: Vec<String>,
    pub mcp_tools: Vec<String>,
    pub skills: Vec<String>,
}

/// Its allowed in and out RPC (the session record keeps them; the gateway's tunnel enforces `out`, `forbidden` and
/// `web_read`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Connections {
    pub out: Vec<Endpoint>,
    #[serde(rename = "in")]
    pub inbound: Vec<Endpoint>,
    /// Hosts it must never reach, even through web read.
    pub forbidden: Vec<String>,
    /// HTTPS GET to any public host, logged per host (DESIGN-12 wall 3).
    pub web_read: bool,
    /// MCP servers: `legionctl` or names in `mcp.servers`.
    pub mcp: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub host: String,
    #[serde(default = "https")]
    pub port: u16,
    /// The TLS server name when it is not the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sni: Option<String>,
    #[serde(default)]
    pub why: String,
}

fn https() -> u16 {
    443
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Limits; none: no limit of this set. The grant keeps the smallest of its sets.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Budgets {
    pub cpus: Option<u32>,
    pub memory_mib: Option<u64>,
    pub seconds: Option<u64>,
    pub storage_mib: Option<u64>,
    pub network_mib: Option<u64>,
    pub tokens: Option<u64>,
    pub usd: Option<f64>,
}

/// A harness's profile (DESIGN-12 "the gateway knows each harness").
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Harness {
    /// The agent name (`session.agents`, `session.acp`) whose sessions request this set by themselves.
    pub agent: String,
    /// The harness versions this profile is for.
    pub versions: String,
    pub model_apis: Vec<ModelApi>,
    /// The variables that point it at a model API (the gateway sets them).
    pub base_url_variables: Vec<String>,
    /// Its login files (`~/`): the real files in a session (DESIGN-12 wall 1).
    pub credentials: Vec<String>,
    /// Telemetry and update hosts, each with what turns it off.
    pub telemetry: Vec<Telemetry>,
    /// More variables in the session.
    pub env: BTreeMap<String, String>,
    /// More arguments after its command.
    pub args: Vec<String>,
    /// Merged into the session's Claude Code settings file.
    pub settings: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ModelApi {
    pub host: String,
    /// `anthropic-messages`, `openai-responses` or `openai-chat`.
    pub shape: String,
    /// `sse`, `websocket`.
    pub streaming: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Telemetry {
    pub host: String,
    /// `NAME=value` that turns it off.
    pub env: Option<String>,
    /// Arguments that turn it off.
    pub args: Vec<String>,
}

/// The JSON schema of a manifest, as `docs/api/runnable.schema.json` holds it.
///
/// # Errors
/// The schema does not serialize.
pub fn schema() -> Result<(&'static str, Value)> {
    Ok((
        "docs/api/runnable.schema.json",
        serde_json::to_value(schemars::schema_for!(Manifest))?,
    ))
}

impl Manifest {
    /// The powerful parts it sets: a repository's set gets them only after the user allowed its content.
    #[must_use]
    pub fn powerful(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.host != Host::default() {
            out.push("host");
        }
        if !self.storage.is_empty() {
            out.push("storage");
        }
        if self.toolverse != Toolverse::default() {
            out.push("toolverse");
        }
        if self.connections != Connections::default() {
            out.push("connections");
        }
        if self.harness.is_some() {
            out.push("harness");
        }
        out
    }
}

/// Whether a set may be used, by the layer it comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Trust {
    /// Shipped, `/etc/legionctl` or the user's: `actions.allow` decides.
    Layer,
    /// A repository's set with no powerful part.
    Plain,
    /// A repository's set whose content the user allowed.
    Allowed,
    /// A repository's set with a powerful part that waits for the user's first answer.
    Waits,
    /// A repository's set whose content the user denied (a changed file asks again).
    Denied,
}

/// A set as the layers give it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Loaded {
    pub set: Manifest,
    pub source: Source,
    pub trust: Trust,
}

/// Read the sets of the layers, lowest first: shipped, then `(layer, folder)` of `/etc/legionctl`, the user and the
/// repository. A higher trusted layer replaces a set of the same name; a repository set may only add a name. The
/// repository's sets with powerful parts and no answer yet are `unapproved` (the operator asks).
///
/// # Errors
/// A file does not parse, its name is not its file's name, or a folder cannot be read (a typo must not drop a set).
pub(crate) fn load(
    layers: &[(&str, PathBuf)],
    approvals: &Approvals,
    problems: &mut Vec<String>,
    unapproved: &mut Vec<Unapproved>,
) -> Result<Vec<Loaded>> {
    let mut out: Vec<Loaded> = Vec::new();
    let mut put = |loaded: Loaded, problems: &mut Vec<String>| {
        match out.iter_mut().find(|l| l.set.name == loaded.set.name) {
            Some(lower) if loaded.source.layer == "repo" => problems.push(format!(
                "{}: capability set {} is already defined in {}; a repository set may only add a name, so it is not used",
                loaded.source.file, loaded.set.name, lower.source.file
            )),
            Some(lower) => *lower = loaded,
            None => out.push(loaded),
        }
    };
    for text in SHIPPED {
        let set: Manifest =
            serde_json::from_reader(json_comments::StripComments::new(text.as_bytes()))
                .context("a shipped capability set does not parse")?;
        let source = Source {
            layer: "defaults".into(),
            file: format!("built in {}", set.name),
        };
        put(
            Loaded {
                set,
                source,
                trust: Trust::Layer,
            },
            problems,
        );
    }
    for (layer, dir) in layers {
        let dir = dir.join("capabilities");
        let entries = match std::fs::read_dir(&dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            entries => entries
                .and_then(Iterator::collect::<std::io::Result<Vec<_>>>)
                .with_context(|| format!("listing {}", dir.display()))?,
        };
        let mut files: Vec<PathBuf> = entries
            .iter()
            .map(std::fs::DirEntry::path)
            .filter(|p| p.extension().is_some_and(|x| x == "jsonc"))
            .collect();
        files.sort();
        for file in files {
            let value = legionctl_machine::profile::read_jsonc(&file)?;
            let set: Manifest = serde_json::from_value(value.clone())
                .with_context(|| format!("{} is not a valid capability set", file.display()))?;
            let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned());
            ensure!(
                stem.as_deref() == Some(set.name.as_str()),
                "{}: its name is {}; a set's name is its file's name",
                file.display(),
                set.name
            );
            let source = Source {
                layer: (*layer).into(),
                file: file.display().to_string(),
            };
            let trust = if *layer != "repo" {
                Trust::Layer
            } else if set.powerful().is_empty() {
                Trust::Plain
            } else {
                let (name, hash, answer) = approvals.of(&file, &value);
                match answer {
                    Some(true) => Trust::Allowed,
                    Some(false) => Trust::Denied,
                    None => {
                        unapproved.push(Unapproved {
                            file: name,
                            hash,
                            keys: set
                                .powerful()
                                .iter()
                                .map(|p| format!("capabilities.{}.{p}", set.name))
                                .collect(),
                        });
                        Trust::Waits
                    }
                }
            };
            put(Loaded { set, source, trust }, problems);
        }
    }
    Ok(out)
}

/// Why a set cannot be used now, when it cannot; `None`: it may be granted.
#[must_use]
pub fn refusal(config: &Config, name: &str) -> Option<String> {
    let Some(loaded) = config.capabilities.iter().find(|l| l.set.name == name) else {
        return Some(
            "no such capability set (config/capabilities, /etc/legionctl/capabilities, \
             ~/.config/legionctl/capabilities, .legionctl/capabilities)"
                .into(),
        );
    };
    if let Some(rule) = config.actions.forbidden("toolset", name) {
        return Some(format!("it is forbidden: {} (actions.forbid)", rule.reason));
    }
    match loaded.trust {
        Trust::Layer if !config.actions.allowed("toolset", name) => {
            Some("your settings do not allow it (actions.allow)".into())
        }
        Trust::Waits => Some(format!(
            "{} sets {} and waits for your first answer (legionctl asks once)",
            loaded.source.file,
            loaded.set.powerful().join(", ")
        )),
        Trust::Denied => Some(format!(
            "you denied the content of {} (a changed file asks again)",
            loaded.source.file
        )),
        Trust::Layer | Trust::Plain | Trust::Allowed => None,
    }
}

/// A set in a grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Granted {
    pub name: String,
    pub version: u32,
    pub layer: String,
    pub file: String,
}

/// What a runnable got at its start: what it asked for, its parent's sets, the sets and their union.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Grant {
    /// `session:<id>`.
    pub runnable: String,
    pub requested: Vec<String>,
    /// A member's parent (the main agent's sets); none: the user is the parent.
    pub parent: Option<Vec<String>>,
    pub sets: Vec<Granted>,
    pub manifest: Manifest,
}

/// What a start asks for.
pub struct Request<'a> {
    pub agent: &'a str,
    pub dir: &'a Path,
    pub inside: Inside,
    /// The sets it names (its archetype's config, its member config, the start's flags).
    pub names: &'a [String],
    pub parent: Option<&'a [String]>,
}

/// The sets a start of `agent` in `dir` gets asked for by itself: its harness's set, and the set of each language of
/// the repository's top folder (DESIGN-12 wall 9).
fn own_sets(config: &Config, agent: &str, dir: &Path) -> Result<Vec<String>> {
    let mut out: Vec<String> = config
        .capabilities
        .iter()
        .filter(|l| l.set.harness.as_ref().is_some_and(|h| h.agent == agent))
        .map(|l| l.set.name.clone())
        .collect();
    let top = crate::config::repo_root(dir).unwrap_or_else(|| dir.to_path_buf());
    for language in crate::checks::languages()? {
        if language.manifests.iter().any(|m| top.join(m).is_file()) {
            out.push(language.capability_set.clone());
        }
    }
    Ok(out)
}

/// Grant `request` for `runnable`: each set that the user layer (or a repository set's first-use answer), the
/// environment and the parent allow, and their union.
///
/// # Errors
/// A set is refused (each with its reason, all in one error: the start must not go on without it), two harnesses, or a
/// language file does not parse.
pub fn grant(config: &Config, runnable: &str, request: &Request) -> Result<Grant> {
    let own = own_sets(config, request.agent, request.dir)?;
    let mut requested: Vec<String> = Vec::new();
    for name in own.iter().chain(request.names) {
        if !requested.contains(name) {
            requested.push(name.clone());
        }
    }
    let hidden: Vec<PathBuf> = config
        .session
        .hidden
        .iter()
        .map(|h| crate::config::expand_home(h))
        .collect::<Result<_>>()?;
    let mut refused = Vec::new();
    let mut sets = Vec::new();
    let mut manifest = Manifest {
        name: runnable.into(),
        ..Manifest::default()
    };
    for name in &requested {
        let mut no = |why: String| refused.push(format!("{name}: {why}"));
        if let Some(why) = refusal(config, name) {
            no(why);
            continue;
        }
        let loaded = config
            .capabilities
            .iter()
            .find(|l| &l.set.name == name)
            .context("a set that refusal found")?;
        let set = &loaded.set;
        if let Some(parent) = request.parent
            && !own.contains(name)
            && !parent.contains(name)
        {
            no("its parent (the main agent) does not have it".into());
        }
        if !set.sandbox.is_empty() && !set.sandbox.contains(&request.inside) {
            let kinds: Vec<&str> = set.sandbox.iter().map(|k| k.as_str()).collect();
            no(format!(
                "it runs only in {}; this session runs in {}",
                kinds.join(", "),
                request.inside.as_str()
            ));
        }
        for problem in host_problems(set, request.inside, &hidden)? {
            no(problem);
        }
        if set.harness.is_some() && manifest.harness.is_some() {
            no("a second harness: a runnable has one".into());
        }
        merge(&mut manifest, set);
        sets.push(Granted {
            name: name.clone(),
            version: set.version,
            layer: loaded.source.layer.clone(),
            file: loaded.source.file.clone(),
        });
    }
    if !refused.is_empty() {
        bail!("capability sets refused at start: {}", refused.join("; "));
    }
    Ok(Grant {
        runnable: runnable.into(),
        requested,
        parent: request.parent.map(<[String]>::to_vec),
        sets,
        manifest,
    })
}

/// What the environment cannot give of a set's host and storage parts.
fn host_problems(set: &Manifest, inside: Inside, hidden: &[PathBuf]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for p in &set.host.privileges {
        out.push(format!(
            "privilege {p}: the sandbox grants no privileges (cap-drop ALL, no new privileges)"
        ));
    }
    for d in &set.host.devices {
        if !d.starts_with("/dev/") || d.contains("..") {
            out.push(format!("device {d} is not a /dev node"));
        } else if inside == Inside::Vm {
            out.push(format!(
                "device {d}: a VM gets no host device nodes (only the GPU through VFIO, --gpu)"
            ));
        }
    }
    for s in &set.storage {
        let ok = (s.path.starts_with('/') || s.path.starts_with("~/"))
            && !Path::new(&s.path)
                .components()
                .any(|c| c == std::path::Component::ParentDir);
        if !ok {
            out.push(format!(
                "storage {}: write an absolute path or one in ~/, without ..",
                s.path
            ));
            continue;
        }
        let path = crate::config::expand_home(&s.path)?;
        if let Some(h) = hidden
            .iter()
            .find(|h| path.starts_with(h) || h.starts_with(&path))
        {
            out.push(format!(
                "storage {}: it reaches {}, which sessions hide (session.hidden)",
                s.path,
                h.display()
            ));
        }
    }
    Ok(out)
}

fn union<T: Clone + PartialEq>(into: &mut Vec<T>, from: &[T]) {
    for item in from {
        if !into.contains(item) {
            into.push(item.clone());
        }
    }
}

fn smallest<T: PartialOrd + Copy>(into: &mut Option<T>, from: Option<T>) {
    if let Some(f) = from
        && into.is_none_or(|i| f < i)
    {
        *into = Some(f);
    }
}

/// Put `set` into the union: rights add up, limits take the smallest, sandbox kinds keep those that all allow.
fn merge(into: &mut Manifest, set: &Manifest) {
    if !set.sandbox.is_empty() {
        into.sandbox = if into.sandbox.is_empty() {
            set.sandbox.clone()
        } else {
            into.sandbox
                .iter()
                .copied()
                .filter(|k| set.sandbox.contains(k))
                .collect()
        };
    }
    union(&mut into.host.privileges, &set.host.privileges);
    union(&mut into.host.devices, &set.host.devices);
    union(
        &mut into.host.forbidden_commands,
        &set.host.forbidden_commands,
    );
    union(&mut into.storage, &set.storage);
    union(&mut into.channels, &set.channels);
    let (t, s) = (&mut into.toolverse, &set.toolverse);
    union(&mut t.classes, &s.classes);
    union(&mut t.operations, &s.operations);
    union(&mut t.mcp_tools, &s.mcp_tools);
    union(&mut t.skills, &s.skills);
    let (c, s) = (&mut into.connections, &set.connections);
    union(&mut c.out, &s.out);
    union(&mut c.inbound, &s.inbound);
    union(&mut c.forbidden, &s.forbidden);
    union(&mut c.mcp, &s.mcp);
    c.web_read |= s.web_read;
    union(&mut into.tools, &set.tools);
    let (b, s) = (&mut into.budgets, &set.budgets);
    smallest(&mut b.cpus, s.cpus);
    smallest(&mut b.memory_mib, s.memory_mib);
    smallest(&mut b.seconds, s.seconds);
    smallest(&mut b.storage_mib, s.storage_mib);
    smallest(&mut b.network_mib, s.network_mib);
    smallest(&mut b.tokens, s.tokens);
    smallest(&mut b.usd, s.usd);
    if set.harness.is_some() {
        into.harness.clone_from(&set.harness);
    }
}

/// A toolset (DESIGN/22 rule 2) holds a tool: its operations or `mcp_tools` name it (`x.*` a group, `server/*` a
/// server's tools, `*/*` every brokered tool), or it has the operation's class (`read`, `session`, `workspace`, `machine`).
#[must_use]
pub fn holds(toolverse: &Toolverse, name: &str, class: Option<&str>) -> bool {
    let named = |pattern: &String| {
        pattern == name
            || pattern
                .strip_suffix(".*")
                .is_some_and(|group| name.starts_with(&format!("{group}.")))
            || pattern.strip_suffix("/*").is_some_and(|server| {
                server == "*" && name.contains('/') || name.starts_with(&format!("{server}/"))
            })
    };
    class.is_some_and(|c| toolverse.classes.iter().any(|x| x == c))
        || toolverse.operations.iter().any(named)
        || toolverse.mcp_tools.iter().any(named)
}

/// The toolset to ask for when a scope does not hold `name`: a set that may be granted and names the tool (or its
/// group or server) first, else one that has its class.
#[must_use]
pub fn toolset_for<'a>(config: &'a Config, name: &str, class: Option<&str>) -> Option<&'a Loaded> {
    let grantable = || {
        config
            .capabilities
            .iter()
            .filter(|l| refusal(config, &l.set.name).is_none())
    };
    grantable()
        .find(|l| holds(&l.set.toolverse, name, None))
        .or_else(|| grantable().find(|l| holds(&l.set.toolverse, name, class)))
}

/// The scope of an agent that declared no toolsets (DESIGN/22, lead decision 3): the read class; the rest is asked.
#[must_use]
pub fn read_only() -> Toolverse {
    Toolverse {
        classes: vec!["read".into()],
        ..Toolverse::default()
    }
}

/// `grant` with `set` added: its name and the union with its manifest.
fn with_set(mut grant: Grant, set: &Loaded) -> Grant {
    if !grant.sets.iter().any(|g| g.name == set.set.name) {
        grant.requested.push(set.set.name.clone());
        merge(&mut grant.manifest, &set.set);
        grant.sets.push(Granted {
            name: set.set.name.clone(),
            version: set.set.version,
            layer: set.source.layer.clone(),
            file: set.source.file.clone(),
        });
    }
    grant
}

/// The connections that session `id` was granted, from its record, as the gateway's tunnel enforces them; `None`: no
/// such session, or one from before grants.
///
/// # Errors
/// The record cannot be read or does not parse.
pub fn connections_of(db: &crate::db::Db, id: &str) -> Result<Option<Connections>> {
    let text: Option<String> = db
        .conn()
        .query_row(
            "SELECT connections FROM sessions WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    text.map(|t| serde_json::from_str(&t).context("a session's granted connections"))
        .transpose()
}

impl Connections {
    /// Why `host`:`port` is not open to this grant, if it is not: a forbidden host never is; a declared endpoint is;
    /// with web read any host on 443 is, when its addresses are all public ([`public`], the caller checks them).
    #[must_use]
    pub fn refusal(&self, host: &str, port: u16) -> Option<String> {
        if self.forbidden.iter().any(|h| h.eq_ignore_ascii_case(host)) {
            return Some("this session may never reach it".into());
        }
        if self.declares(host, port) || (self.web_read && port == 443) {
            return None;
        }
        Some("this session has no connection to it (`legionctl runnables show <session>`)".into())
    }

    /// Whether `host`:`port` is one of its declared endpoints.
    #[must_use]
    pub fn declares(&self, host: &str, port: u16) -> bool {
        self.out
            .iter()
            .any(|e| e.host.eq_ignore_ascii_case(host) && e.port == port)
    }
}

/// An address on the open internet: not loopback, private, link-local, shared (100.64/10), unspecified, broadcast,
/// documentation or unique local.
#[must_use]
pub fn public(address: std::net::IpAddr) -> bool {
    match address {
        std::net::IpAddr::V4(a) => {
            let [first, second, ..] = a.octets();
            !(a.is_loopback()
                || a.is_private()
                || a.is_link_local()
                || a.is_unspecified()
                || a.is_broadcast()
                || a.is_documentation()
                || (first == 100 && (64..128).contains(&second)))
        }
        std::net::IpAddr::V6(a) => {
            let first = a.segments()[0];
            a.to_ipv4_mapped().map_or_else(
                || {
                    !(a.is_loopback()
                        || a.is_unspecified()
                        || (first & 0xfe00) == 0xfc00
                        || (first & 0xffc0) == 0xfe80)
                },
                |v4| public(v4.into()),
            )
        }
    }
}

/// The fact entity of session `id`'s grant.
#[must_use]
pub fn entity(id: &str) -> String {
    format!("runnable:session:{id}")
}

impl crate::dossier::Dossiers {
    /// Keep `grant` of session `id`: a fact (the manifest, with time), and its connections in the session's record,
    /// where the gateway reads them.
    ///
    /// # Errors
    /// A write fails.
    pub fn keep_grant(&self, id: &str, grant: &Grant) -> Result<()> {
        let now = crate::facts::now_ms();
        self.fact_add(&crate::facts::New {
            entity: entity(id),
            attribute: "manifest".into(),
            value: serde_json::to_value(grant)?,
            scope: (crate::facts::Scope::Global, String::new()),
            source: "rule".into(),
            evidence: serde_json::json!({ "requested": grant.requested }),
            confidence: 1.0,
            valid_from: now,
            valid_to: None,
            repo: None,
            from_ref: None,
            to_ref: None,
            anchor: None,
        })?;
        self.db().execute(
            "UPDATE sessions SET connections = ?1 WHERE id = ?2",
            rusqlite::params![serde_json::to_string(&grant.manifest.connections)?, id],
        )?;
        Ok(())
    }

    /// The scope of agent session `id` (a sandboxed session, or a harness agent by its session id): its grant, or
    /// for an agent without one the read class.
    ///
    /// # Errors
    /// The grant cannot be read or does not parse.
    pub fn scope(&self, id: &str) -> Result<Grant> {
        Ok(match self.fact_value(&entity(id), "manifest", &[])? {
            Some(value) => serde_json::from_value(value)?,
            None => Grant {
                runnable: format!("session:{id}"),
                requested: Vec::new(),
                parent: None,
                sets: Vec::new(),
                manifest: Manifest {
                    name: format!("session:{id}"),
                    toolverse: read_only(),
                    ..Manifest::default()
                },
            },
        })
    }

    /// Add the toolset `set` to the scope of session `id` (the user allowed it, or a skill that the agent loaded
    /// declares it).
    ///
    /// # Errors
    /// The grant cannot be read or written.
    pub fn grant_toolset(&self, id: &str, set: &Loaded) -> Result<()> {
        let grant = self.scope(id)?;
        if grant.sets.iter().any(|g| g.name == set.set.name) {
            return Ok(());
        }
        self.keep_grant(id, &with_set(grant, set))
    }

    /// The grant of session `id`, or of member `<thread>/<member>` (its session).
    ///
    /// # Errors
    /// No such member or session, or no grant (a session that started before grants, or no legionctl session).
    pub fn grant_of(&self, target: &str) -> Result<Grant> {
        let id = match target.split_once('/') {
            Some((thread, member)) => self
                .db()
                .query_row(
                    "SELECT session FROM members WHERE thread = ?1 AND name = ?2",
                    rusqlite::params![thread, member],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten()
                .with_context(|| {
                    format!("member {target} has no session yet (it starts at its first wake)")
                })?,
            None => target.to_string(),
        };
        let value = self
            .fact_value(&entity(&id), "manifest", &[])?
            .with_context(|| {
                format!(
                    "session {id} has no granted manifest: it started before grants, or it is no legionctl session"
                )
            })?;
        Ok(serde_json::from_value(value)?)
    }
}
