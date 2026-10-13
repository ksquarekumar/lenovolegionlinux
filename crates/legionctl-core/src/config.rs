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
    pub load: Thresholds,
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
    /// The stdio server's command line; empty when `package` gives it.
    #[serde(default)]
    pub command: Vec<String>,
    /// A package from the store: its `run` line, from the hash-checked folder, without network unless it allows it.
    #[serde(default)]
    pub package: Option<String>,
    /// Tools the agent may call; empty: every tool the server offers.
    #[serde(default)]
    pub allow: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionConfig {
    /// Agent name -> command line; `{id}` is the session id, `{settings}` the generated Claude Code settings file and
    /// `{mcp}` the generated MCP config (the proxied servers and legionctl's own).
    pub agents: BTreeMap<String, Vec<String>>,
    /// Writable in the sandbox besides the workspace, the dossier and the repository's .git (`~/` is the home).
    pub writable: Vec<String>,
    /// Hidden in the sandbox: sockets and folders that would give a way out (a container or VM with the host's
    /// files). A folder becomes empty, a file or socket `/dev/null`.
    pub hidden: Vec<String>,
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
    pub gate: bool,
    /// At session start, bind the session to the last dossier used in its repository (or folder) when no live
    /// session holds it.
    pub auto_bind: bool,
    /// Hook events at which the agent is told to set up a session that has no dossier.
    pub nudge: Vec<Event>,
    pub stale_hours: u64,
    /// Class name -> purpose.
    pub classes: BTreeMap<String, String>,
    pub plan_sections: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    /// Layer names, lowest first.
    pub precedence: Vec<String>,
    /// Keys or key prefixes that only the defaults and /etc/legionctl may set.
    pub locked: Vec<String>,
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
    user_dir: PathBuf,
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

/// `*.jsonc` files of a layer folder in merge order: by name, and the managed file last.
fn layer_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonc"))
        .collect();
    files.sort_by_key(|p| (p.file_name().is_some_and(|n| n == MANAGED), p.clone()));
    files
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

/// Resolve the config for work in `project` (its repository layer applies), or without a repository.
///
/// # Errors
/// The built-in defaults do not parse, `/etc/legionctl` sets an unknown layer, or the merged config does not match
/// [`Config`] (the message names the key). A file that is not valid JSONC is a problem, not an error.
pub fn resolve(dirs: &Dirs, project: Option<&Path>) -> Result<Resolved> {
    let defaults: Value =
        serde_json::from_reader(json_comments::StripComments::new(DEFAULTS.as_bytes()))
            .context("parsing the built-in defaults")?;
    let system_dir = dirs.system.clone();
    let user_dir = dirs.user.clone();
    let repo_dir = project.and_then(repo_root).map(|r| r.join(".legionctl"));

    let mut problems = Vec::new();
    let mut read = |dir: &Path| -> Vec<(PathBuf, Value)> {
        layer_files(dir)
            .into_iter()
            .filter_map(|file| match read_jsonc(&file) {
                Ok(Value::Object(map)) => Some((file, Value::Object(map))),
                Ok(_) => {
                    problems.push(format!("{}: not a JSON object; ignored", file.display()));
                    None
                }
                Err(e) => {
                    problems.push(format!("{e:#}; ignored"));
                    None
                }
            })
            .collect()
    };
    let system = read(&system_dir);
    let user = read(&user_dir);
    let repo = repo_dir.as_deref().map(&mut read).unwrap_or_default();

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

    let mut merged = Map::new();
    let mut trace = BTreeMap::new();
    for (source, trusted, value) in ordered {
        if let Value::Object(map) = value {
            Merge {
                source: &source,
                trusted,
                locks: &locks,
                trace: &mut trace,
                problems: &mut problems,
            }
            .object(&mut merged, map, "");
        }
    }
    let value = Value::Object(merged);
    let config: Config =
        serde_json::from_value(value.clone()).context("the merged config is not valid")?;
    Ok(Resolved {
        config,
        value,
        layers,
        trace,
        problems,
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
