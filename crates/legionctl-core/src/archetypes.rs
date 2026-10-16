//! Agent archetypes, configs and teams (design 23, B+2 to B+4). One agent is an archetype file
//! (`<plugin>/agents/<archetype>.md`, the lowest layer), its config `agents.configs.<name>` above it, and a team
//! member above that. A higher layer may only narrow tools, commands and budgets. Each member runs on one run path,
//! and a field that its path cannot apply refuses the member, with the field named. Codex gets each config as an
//! agent file (`~/.codex/agents/<name>.toml`) with its instructions, model and effort only.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::thread::{Archetype as Role, Kind, Pace};

/// Frontmatter fields that Claude Code honours for a plugin agent and that legionctl ships (B+3); the plugin path
/// ignores `permissionMode`, `hooks`, `mcpServers` and `initialPrompt`.
const FRONTMATTER: [&str; 8] = [
    "name",
    "description",
    "tools",
    "disallowedTools",
    "model",
    "effort",
    "skills",
    "background",
];
/// How an archetype file names a ref; Claude Code puts the plugin's folder in its place.
const ROOT_VAR: &str = "${CLAUDE_PLUGIN_ROOT}/";
/// The first line of each Codex agent file that legionctl writes; setup removes such a file when its config is gone.
const CODEX_HEADER: &str = "# Written by `legionctl setup apply`";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    Claude,
    Codex,
}

/// Tools the agent may call (`allow`, empty: each tool) and may not call (`deny`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ToolRules {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

/// Command patterns (`Bash(cargo publish*)`) the agent may run, may not run, and runs only after a person agrees.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CommandRules {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub ask: Vec<String>,
}

/// Token and time limits: per day, and per wake of a member that sleeps until a trigger.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AgentBudget {
    pub tokens_per_day: Option<u64>,
    pub wake_tokens: Option<u64>,
    pub wake_seconds: Option<u64>,
}

/// One layer of an agent: a config (`agents.configs.<name>`, which names its `archetype`) or a team member (which
/// names its config or archetype with `agent`). Each field is optional; a layer sets only what it changes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    /// A config: its archetype (`main-agent`, `thread-observer`, `ui-composer`).
    pub archetype: Option<String>,
    /// A team member: the config (or archetype) that it uses.
    pub agent: Option<String>,
    /// The harness that this config is for; none: both.
    pub harness: Option<Harness>,
    /// An `agents accounts` profile.
    pub account: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub tools: ToolRules,
    /// Skills to add (names of legionctl skills).
    pub skills: Vec<String>,
    /// MCP servers to add: `legionctl` or names in `mcp.servers`.
    pub mcp: Vec<String>,
    pub commands: CommandRules,
    /// Instruction files to add (`~/` is the home; a relative path is from the repository root).
    pub instructions: Vec<String>,
    /// `on-trigger`, `on-stop` or `on-declare`.
    pub pace: Option<String>,
    pub budget: AgentBudget,
    /// Codex sandbox mode of an operator session.
    pub sandbox: Option<String>,
    /// Codex approval policy of an operator session.
    pub approval: Option<String>,
    /// What answers a wake of an operator member, in order (todo-138): `rlcd` (a System One decision on the events;
    /// escalate wakes the next step with its answer as a brief) and `acp` (the member's session). A step that is not
    /// configured is skipped; a step that fails is reported, and the next step does not run in its place. None:
    /// `acp`.
    pub route: Vec<String>,
    /// The rlcd step of the route.
    pub rlcd: Option<MemberRlcd>,
    /// Capability sets it requests at its start (DESIGN-13); a higher layer may only narrow them.
    pub capabilities: Vec<String>,
}

/// A member's rlcd step: its backend (none: `rlcd.backend`) and questions. The question `action` (a choice of
/// ignore, note and escalate) decides; with `escalate_at`, an escalate probability of at least that percent escalates
/// too.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemberRlcd {
    #[serde(default)]
    pub backend: Option<String>,
    pub questions: BTreeMap<String, crate::rlcd::Question>,
    #[serde(default)]
    pub escalate_at: Option<u8>,
}

/// The options of the question `action` of a member's rlcd step.
pub const ACTIONS: [&str; 3] = ["escalate", "ignore", "note"];

/// A team file (`teams/<name>.jsonc`): the members of a thread and the flow between them.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Team {
    #[serde(rename = "$schema", default)]
    pub schema: Option<String>,
    pub name: String,
    /// Member name -> its layer: `agent` names a config or an archetype; the other fields narrow it.
    pub members: BTreeMap<String, AgentConfig>,
    #[serde(default)]
    pub flow: Flow,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Flow {
    /// Member -> the message kinds it may send; only kinds that its archetype may send.
    pub may: BTreeMap<String, Vec<String>>,
    /// Gate -> its conditions, each `<member>: no open hard-block` or `<member>: result <first words>`. Gates:
    /// `checkpoint` (`workspace checkpoint` of the main agent), `declare:done` (`thread declare done`),
    /// `task:completed` (a teammate's task in team mode).
    pub gates: BTreeMap<String, Vec<String>>,
    /// When the pool ends (its members end, its log stays): `main0 ended` (the main agent's session ended),
    /// `declare:done` (main0 declared done and the gate passed). With `budget`, a member that used its
    /// `budget.tokens_per_day` gets no wake until the next day.
    pub stop: Vec<String>,
    /// How often a member whose wake failed starts again; past it, the member fails and its blocks expire.
    pub restart: Restart,
    /// Watch rules that the operator adds at the start: a member watches another, and a call of `tools` on `path`
    /// with no Bash command that contains `expect` before the reply ends (the stop) is a miss that wakes it.
    pub rules: Vec<FlowRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Restart {
    pub max: u32,
    /// `<n>s`, `<n>m` or `<n>h`.
    pub within: String,
}

impl Default for Restart {
    fn default() -> Restart {
        Restart {
            max: 3,
            within: "1h".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowRule {
    /// The member that watches and does the step (or nudges).
    pub member: String,
    #[serde(default = "main0")]
    pub watched: String,
    #[serde(default = "edit_tools")]
    pub tools: String,
    #[serde(default)]
    pub path: String,
    /// The Bash command that the watched member should run (`make lint`).
    pub expect: String,
}

fn main0() -> String {
    "main0".into()
}

fn edit_tools() -> String {
    "Edit|Write|MultiEdit|NotebookEdit".into()
}

/// Seconds of a duration `<n>s`, `<n>m` or `<n>h`.
///
/// # Errors
/// Another form.
pub fn seconds(text: &str) -> Result<u64> {
    let (n, unit) = text.split_at(text.len().saturating_sub(1));
    let n: u64 = n
        .parse()
        .with_context(|| format!("{text} is not <n>s, <n>m or <n>h"))?;
    Ok(match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        _ => anyhow::bail!("{text} is not <n>s, <n>m or <n>h"),
    })
}

/// One gate condition: the member it names and what it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    NoHardBlock(String),
    /// The member's newest `result` message starts with these words.
    Result(String, String),
}

/// # Errors
/// A condition of another form.
pub fn condition(text: &str) -> Result<Condition> {
    let (member, need) = text
        .split_once(": ")
        .with_context(|| format!("gate condition '{text}' is not '<member>: <need>'"))?;
    match need.strip_prefix("result ") {
        Some(words) => Ok(Condition::Result(member.into(), words.into())),
        None if need == "no open hard-block" => Ok(Condition::NoHardBlock(member.into())),
        None => anyhow::bail!(
            "gate condition '{text}': use '<member>: no open hard-block' or '<member>: result <words>'"
        ),
    }
}

const GATES: [&str; 3] = ["checkpoint", "declare:done", "task:completed"];
const STOPS: [&str; 3] = ["main0 ended", "declare:done", "budget"];

impl Flow {
    /// What in the flow does not exist: a gate, a condition form, a stop, a member, a duration.
    fn problems(&self, members: &BTreeMap<String, AgentConfig>) -> Vec<String> {
        let mut out = Vec::new();
        let named = |m: &str, out: &mut Vec<String>, what: &str| {
            if !members.contains_key(m) {
                out.push(format!("{what} names {m}, which is not a member"));
            }
        };
        for (gate, conditions) in &self.gates {
            if !GATES.contains(&gate.as_str()) {
                out.push(format!("flow.gates: no gate {gate} ({})", GATES.join(", ")));
            }
            for c in conditions {
                match condition(c) {
                    Ok(Condition::NoHardBlock(m) | Condition::Result(m, _)) => {
                        named(&m, &mut out, &format!("flow.gates.{gate}"));
                    }
                    Err(e) => out.push(format!("flow.gates.{gate}: {e}")),
                }
            }
        }
        for stop in self.stop.iter().filter(|s| !STOPS.contains(&s.as_str())) {
            out.push(format!(
                "flow.stop: no condition '{stop}' ({})",
                STOPS.join(", ")
            ));
        }
        if let Err(e) = seconds(&self.restart.within) {
            out.push(format!("flow.restart.within: {e}"));
        }
        for rule in &self.rules {
            named(&rule.member, &mut out, "flow.rules");
            named(&rule.watched, &mut out, "flow.rules");
        }
        out
    }
}

/// The JSON schema of team files, with its repository path.
///
/// # Errors
/// The schema does not serialize.
pub fn team_schema() -> Result<(&'static str, Value)> {
    Ok((
        "integrations/agent-plugin/teams/team.schema.json",
        serde_json::to_value(schemars::schema_for!(Team))?,
    ))
}

/// Where a member runs: its own operator session, a Claude Code teammate (a plugin agent type of the lead), or a
/// Codex subagent of the main Codex session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunPath {
    ClaudeSession,
    ClaudeTeam,
    CodexSession,
    CodexSubagent,
}

impl RunPath {
    fn of(harness: Harness, main: bool) -> RunPath {
        match (harness, main) {
            (Harness::Claude, true) => RunPath::ClaudeSession,
            (Harness::Claude, false) => RunPath::ClaudeTeam,
            (Harness::Codex, true) => RunPath::CodexSession,
            (Harness::Codex, false) => RunPath::CodexSubagent,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            RunPath::ClaudeSession => "claude-session",
            RunPath::ClaudeTeam => "claude-team",
            RunPath::CodexSession => "codex-session",
            RunPath::CodexSubagent => "codex-subagent",
        }
    }

    /// Why this path cannot apply `field` when the layer `from` sets it (B+2, "How the operator applies each field").
    fn refuses(self, field: &str, from: Layer) -> Option<&'static str> {
        use RunPath::{ClaudeSession, ClaudeTeam, CodexSession, CodexSubagent};
        let why = match (self, field) {
            (ClaudeSession | ClaudeTeam, "sandbox" | "approval") => {
                "Claude Code has no sandbox mode or approval policy per agent"
            }
            (CodexSubagent, "sandbox" | "approval") => {
                "a Codex subagent inherits the sandbox and the approval policy of its main session"
            }
            (CodexSession | CodexSubagent, "tools") => {
                "Codex has no allow or deny list of tools; use commands"
            }
            (ClaudeTeam | CodexSubagent, "capabilities") => {
                "a teammate or subagent runs in its lead's sandbox, with the lead's grant"
            }
            (ClaudeSession | CodexSession, _) | (_, "pace" | "budget") | (ClaudeTeam, "model") => {
                return None;
            }
            (ClaudeTeam | CodexSubagent, "route" | "rlcd") => {
                "only an operator member wakes; the route and rlcd decide its wakes"
            }
            (ClaudeTeam, "mcp") => {
                "a teammate is a plugin agent type, and Claude Code ignores mcpServers for plugin agents"
            }
            (ClaudeTeam, "account") => "a teammate uses the account of its lead",
            (ClaudeTeam, _) => {
                "a teammate is the plugin's agent file; a config cannot change its frontmatter or its text"
            }
            (CodexSubagent, "mcp") => {
                "a Codex subagent inherits the MCP servers of its main session"
            }
            (CodexSubagent, "account") => "a Codex subagent uses the account of its main session",
            (CodexSubagent, "commands") => {
                "a Codex subagent inherits the approval policy of its main session"
            }
            (CodexSubagent, _) if from == Layer::Team => {
                "setup apply writes one Codex agent file per config; a team member cannot change it"
            }
            (CodexSubagent, _) => return None,
        };
        // the archetype file is the plugin agent's own file: each field of it applies, but what no Codex path has
        (from != Layer::Archetype || matches!(field, "sandbox" | "approval" | "tools"))
            .then_some(why)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layer {
    Archetype,
    Config,
    Team,
}

/// An archetype file: its frontmatter as the lowest layer, and its instruction text.
#[derive(Debug, Clone)]
pub struct ArchetypeFile {
    pub path: PathBuf,
    pub description: String,
    pub body: String,
    layer: AgentConfig,
}

fn list(value: &str) -> Vec<String> {
    value
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// Read `<plugin>/agents/<name>.md`. Each frontmatter field that the plugin path does not honour, and each ref that
/// does not exist, is a refusal.
fn archetype_file(plugin: &Path, name: &str, refused: &mut Vec<String>) -> Option<ArchetypeFile> {
    let path = plugin.join("agents").join(format!("{name}.md"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        refused.push(format!("{}: no archetype file", path.display()));
        return None;
    };
    let shown = path.display();
    let Some((head, body)) = text
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
    else {
        refused.push(format!("{shown}: no frontmatter between --- lines"));
        return None;
    };
    let mut file = ArchetypeFile {
        path: path.clone(),
        description: String::new(),
        body: body.trim().to_string(),
        layer: AgentConfig::default(),
    };
    for line in head.lines() {
        let Some((key, value)) = line.split_once(':') else {
            refused.push(format!(
                "{shown}: frontmatter line '{line}' is not 'field: value'"
            ));
            continue;
        };
        let value = value.trim();
        let l = &mut file.layer;
        match key.trim() {
            "name" if value != name => {
                refused.push(format!("{shown}: name is {value}, the file is {name}"));
            }
            "description" => file.description = value.to_string(),
            "tools" => l.tools.allow = list(value),
            "disallowedTools" => l.tools.deny = list(value),
            "skills" => l.skills = list(value),
            // `inherit`: the model of the main agent, the same as no model
            "model" if value != "inherit" => l.model = Some(value.to_string()),
            "effort" => l.effort = Some(value.to_string()),
            key if FRONTMATTER.contains(&key) => {}
            key => refused.push(format!(
                "{shown}: frontmatter field {key} is not one that plugin agents honour ({})",
                FRONTMATTER.join(", ")
            )),
        }
    }
    for (at, _) in body.match_indices(ROOT_VAR) {
        let rest = &body[at + ROOT_VAR.len()..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || "._/-".contains(c)))
            .unwrap_or(rest.len());
        let rel = rest[..end].trim_end_matches('.');
        if !plugin.join(rel).is_file() {
            refused.push(format!("{shown}: the ref {rel} does not exist"));
        }
    }
    Some(file)
}

/// The message kinds an archetype may send (B+3).
fn kinds(archetype: Role) -> Vec<Kind> {
    match archetype {
        Role::MainAgent => vec![Kind::Note],
        Role::UiComposer => vec![Kind::Note, Kind::Result],
        Role::ThreadObserver => vec![
            Kind::Note,
            Kind::Hint,
            Kind::Reminder,
            Kind::Notice,
            Kind::Result,
            Kind::SoftBlock,
            Kind::HardBlock,
        ],
    }
}

/// What a higher layer adds to the lower one in tools, commands and budget. `whole`: `higher` is a whole config (an
/// empty allow list allows each item, a missing deny or limit is removed); else it is a layer that sets only what it
/// changes.
fn widenings(lower: &AgentConfig, higher: &AgentConfig, whole: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut allow = |field: &str, low: &[String], high: &[String]| {
        if low.is_empty() {
            return;
        }
        if high.is_empty() && whole {
            out.push(format!(
                "{field} is empty (each item); below it: {}",
                low.join(", ")
            ));
        }
        for item in high.iter().filter(|i| !low.contains(i)) {
            out.push(format!("{field} adds {item}"));
        }
    };
    allow("tools.allow", &lower.tools.allow, &higher.tools.allow);
    allow(
        "commands.allow",
        &lower.commands.allow,
        &higher.commands.allow,
    );
    if whole {
        let gone = |low: &[String], high: &[&Vec<String>]| -> Vec<String> {
            low.iter()
                .filter(|i| !high.iter().any(|h| h.contains(i)))
                .cloned()
                .collect()
        };
        for item in gone(&lower.tools.deny, &[&higher.tools.deny]) {
            out.push(format!("tools.deny removes {item}"));
        }
        for item in gone(&lower.commands.deny, &[&higher.commands.deny]) {
            out.push(format!("commands.deny removes {item}"));
        }
        for item in gone(
            &lower.commands.ask,
            &[&higher.commands.ask, &higher.commands.deny],
        ) {
            out.push(format!("commands.ask removes {item}"));
        }
    }
    let limits = [
        (
            "budget.tokens_per_day",
            lower.budget.tokens_per_day,
            higher.budget.tokens_per_day,
        ),
        (
            "budget.wake_tokens",
            lower.budget.wake_tokens,
            higher.budget.wake_tokens,
        ),
        (
            "budget.wake_seconds",
            lower.budget.wake_seconds,
            higher.budget.wake_seconds,
        ),
    ];
    for (field, low, high) in limits {
        match (low, high) {
            (Some(l), Some(h)) if h > l => out.push(format!("{field} raises {l} to {h}")),
            (Some(l), None) if whole => out.push(format!("{field} removes the limit {l}")),
            _ => {}
        }
    }
    if whole {
        for item in higher
            .capabilities
            .iter()
            .filter(|c| !lower.capabilities.contains(c))
        {
            out.push(format!("capabilities adds {item}"));
        }
    }
    if whole && lower.archetype != higher.archetype {
        out.push("archetype changes".into());
    }
    out
}

fn add(into: &mut Vec<String>, items: &[String]) {
    for item in items {
        if !into.contains(item) {
            into.push(item.clone());
        }
    }
}

/// Put `layer` over `into`: a value replaces, an allow list replaces, deny and ask lists and added lists grow.
/// `set_by` gets the layer of each field that it sets.
fn apply(
    into: &mut AgentConfig,
    layer: &AgentConfig,
    name: &str,
    set_by: &mut BTreeMap<String, String>,
) {
    let mut mark = |field: &str, set: bool| {
        if set {
            set_by.insert(field.into(), name.into());
        }
    };
    let scalars = [
        ("account", &mut into.account, &layer.account),
        ("model", &mut into.model, &layer.model),
        ("effort", &mut into.effort, &layer.effort),
        ("pace", &mut into.pace, &layer.pace),
        ("sandbox", &mut into.sandbox, &layer.sandbox),
        ("approval", &mut into.approval, &layer.approval),
    ];
    for (field, to, from) in scalars {
        mark(field, from.is_some());
        if from.is_some() {
            to.clone_from(from);
        }
    }
    mark("route", !layer.route.is_empty());
    if !layer.route.is_empty() {
        into.route.clone_from(&layer.route);
    }
    mark("capabilities", !layer.capabilities.is_empty());
    if !layer.capabilities.is_empty() {
        into.capabilities.clone_from(&layer.capabilities);
    }
    mark("rlcd", layer.rlcd.is_some());
    if layer.rlcd.is_some() {
        into.rlcd.clone_from(&layer.rlcd);
    }
    mark("tools", layer.tools != ToolRules::default());
    if !layer.tools.allow.is_empty() {
        into.tools.allow.clone_from(&layer.tools.allow);
    }
    add(&mut into.tools.deny, &layer.tools.deny);
    mark("commands", layer.commands != CommandRules::default());
    if !layer.commands.allow.is_empty() {
        into.commands.allow.clone_from(&layer.commands.allow);
    }
    add(&mut into.commands.deny, &layer.commands.deny);
    add(&mut into.commands.ask, &layer.commands.ask);
    for (field, to, from) in [
        ("skills", &mut into.skills, &layer.skills),
        ("mcp", &mut into.mcp, &layer.mcp),
        ("instructions", &mut into.instructions, &layer.instructions),
    ] {
        mark(field, !from.is_empty());
        add(to, from);
    }
    mark("budget", layer.budget != AgentBudget::default());
    let b = &layer.budget;
    for (to, from) in [
        (&mut into.budget.tokens_per_day, b.tokens_per_day),
        (&mut into.budget.wake_tokens, b.wake_tokens),
        (&mut into.budget.wake_seconds, b.wake_seconds),
    ] {
        if from.is_some() {
            *to = from;
        }
    }
}

/// One member with its layers resolved.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Member {
    pub name: String,
    pub archetype: String,
    /// The config (`agents.configs.<name>`) it uses.
    pub config: String,
    /// `claude-session`, `claude-team` (a teammate of the lead), `codex-session` or `codex-subagent`.
    pub run_path: String,
    /// The resolved fields that are set, and `may` (the message kinds it may send).
    pub fields: Map<String, Value>,
    /// Field -> the layer that set it last.
    pub set_by: BTreeMap<String, String>,
}

/// A capability set as `runnables check` shows it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetState {
    pub name: String,
    pub version: u32,
    pub layer: String,
    pub file: String,
    pub trust: crate::runnables::Trust,
    /// Its powerful parts (a repository set gets them after the user's first-use answer).
    pub powerful: Vec<String>,
    /// Why a grant would refuse it now; none: it may be granted.
    pub refusal: Option<String>,
}

/// What `runnables check` found.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Check {
    pub harness: Harness,
    pub team: Option<String>,
    pub members: Vec<Member>,
    /// The capability sets of the layers.
    pub sets: Vec<SetState>,
    /// Each field that a run path cannot apply, each layer that widens, each missing file: one line each.
    pub refused: Vec<String>,
    /// The team's flow (empty without a team).
    #[serde(skip)]
    pub flow: Flow,
}

/// What a check reads.
pub struct Sources {
    /// The plugin folder (`agents/`, `agent-refs/`, `teams/`).
    pub plugin: PathBuf,
    /// The configs with the repository layer, and without it (the repository layer may only narrow).
    pub configs: BTreeMap<String, AgentConfig>,
    pub configs_below_repo: BTreeMap<String, AgentConfig>,
    /// MCP server names in `mcp.servers`.
    pub mcp_servers: Vec<String>,
    /// The repository root, for relative instruction files and the repository's teams.
    pub repo: Option<PathBuf>,
    /// The user's config folder, for the user's teams.
    user_dir: PathBuf,
    /// The config with the repository layer: its capability sets and `runnables.allow`.
    config: crate::config::Config,
    /// The layers' problems with capability sets.
    set_problems: Vec<String>,
}

impl Sources {
    /// The configs of the config layers for work in `project` (none: the user's, without a repository).
    ///
    /// # Errors
    /// The config does not resolve.
    pub fn resolve(dirs: &crate::config::Dirs, project: Option<&Path>) -> Result<Sources> {
        let with = crate::config::resolve(dirs, project)?;
        let below = crate::config::resolve(dirs, None)?;
        Ok(Sources {
            plugin: crate::setup::plugin_root(),
            configs: with.config.agents.configs.clone(),
            configs_below_repo: below.config.agents.configs,
            mcp_servers: with.config.mcp.servers.keys().cloned().collect(),
            repo: project.and_then(crate::config::repo_root),
            user_dir: dirs.user.clone(),
            set_problems: with
                .problems
                .into_iter()
                .filter(|p| p.contains("capability set"))
                .collect(),
            config: with.config,
        })
    }

    /// The first team file named `name`: the repository's `.legionctl/teams/`, the user's `teams/`, then the
    /// plugin's.
    #[must_use]
    pub fn team_path(&self, name: &str) -> Option<PathBuf> {
        let mut dirs: Vec<PathBuf> = self
            .repo
            .iter()
            .map(|r| r.join(".legionctl/teams"))
            .collect();
        dirs.push(self.user_dir.join("teams"));
        dirs.push(self.plugin.join("teams"));
        dirs.into_iter()
            .map(|d| d.join(format!("{name}.jsonc")))
            .find(|p| p.is_file())
    }
}

impl Sources {
    /// One member, from its archetype file up to its team layer.
    fn member(
        &self,
        name: &str,
        layer: &AgentConfig,
        team: Option<&str>,
        (harness, sessions): (Harness, bool),
        refused: &mut Vec<String>,
    ) -> Option<(Member, ArchetypeFile, AgentConfig)> {
        let mut no = |text: String| refused.push(format!("{name}: {text}"));
        let agent = layer.agent.as_deref().unwrap_or(name);
        if layer.archetype.is_some() {
            no("a team member names its config or archetype with agent, not archetype".into());
        }
        let (config, role) = match self.config_of(agent) {
            Ok(found) => found,
            Err(why) => {
                no(why);
                return None;
            }
        };
        let file = archetype_file(&self.plugin, role.as_str(), refused)?;
        let mut no = |text: String| refused.push(format!("{name}: {text}"));
        if let Some(below) = self.configs_below_repo.get(agent) {
            for w in widenings(below, &config, true) {
                no(format!(
                    "the repository layer widens agents.configs.{agent}: {w}"
                ));
            }
        }
        let config_name = format!("agents.configs.{agent}");
        let team_name = team.map(|t| format!("team {t}"));
        for w in widenings(&file.layer, &config, false) {
            no(format!("{config_name} widens the archetype: {w}"));
        }
        let mut set_by = BTreeMap::new();
        let mut merged = AgentConfig::default();
        let mut stack = vec![
            (
                Layer::Archetype,
                file.path.display().to_string(),
                file.layer.clone(),
            ),
            (Layer::Config, config_name.clone(), config.clone()),
        ];
        if let Some(team_name) = &team_name {
            stack.push((Layer::Team, team_name.clone(), layer.clone()));
        }
        let mut from = BTreeMap::new();
        for (kind, label, layer) in &stack {
            if *kind == Layer::Team {
                for w in widenings(&merged, layer, false) {
                    no(format!("{label} widens {w}"));
                }
                for c in layer
                    .capabilities
                    .iter()
                    .filter(|c| !merged.capabilities.contains(c))
                {
                    no(format!("{label} widens capabilities: adds {c}"));
                }
            }
            apply(&mut merged, layer, label, &mut set_by);
            for (field, by) in &set_by {
                if by == label {
                    from.insert(field.clone(), *kind);
                }
            }
        }
        if let Some(h) = config.harness.filter(|h| *h != harness) {
            no(format!(
                "harness: {config_name} is for {}",
                serde_json::to_value(h).unwrap_or_default()
            ));
        }
        let path = RunPath::of(harness, role == Role::MainAgent || sessions);
        for (field, kind) in &from {
            if let Some(why) = path.refuses(field, *kind) {
                no(format!(
                    "{field} (set by {}) cannot apply on run path {}: {why}",
                    set_by[field],
                    path.as_str()
                ));
            }
        }
        for problem in self.value_problems(&merged) {
            no(problem);
        }
        let mut fields = match serde_json::to_value(&merged) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        };
        fields.retain(|_, v| !is_empty(v));
        fields.insert(
            "may".into(),
            kinds(role).iter().map(|k| k.as_str()).collect(),
        );
        let member = Member {
            name: name.into(),
            archetype: role.as_str().into(),
            config: agent.into(),
            run_path: path.as_str().into(),
            fields,
            set_by,
        };
        Some((member, file, merged))
    }

    /// The config named `agent` (else the defaults of the archetype `agent`) and its archetype.
    fn config_of(&self, agent: &str) -> Result<(AgentConfig, Role), String> {
        let config = match self.configs.get(agent) {
            Some(config) => config.clone(),
            None if Role::parse(agent).is_ok() => AgentConfig {
                archetype: Some(agent.into()),
                ..AgentConfig::default()
            },
            None => {
                return Err(format!(
                    "no agents.configs.{agent} and no archetype {agent}"
                ));
            }
        };
        if config.agent.is_some() {
            return Err(format!(
                "agents.configs.{agent} sets agent; a config names its archetype"
            ));
        }
        let role = config
            .archetype
            .as_deref()
            .and_then(|a| Role::parse(a).ok());
        let role = role.ok_or_else(|| {
            format!(
                "agents.configs.{agent}.archetype is {:?}; use main-agent, thread-observer or ui-composer",
                config.archetype
            )
        })?;
        Ok((config, role))
    }

    /// Values that do not exist: a pace, an MCP server, an instruction file.
    fn value_problems(&self, merged: &AgentConfig) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(pace) = merged.pace.as_deref().filter(|p| Pace::parse(p).is_err()) {
            out.push(format!(
                "pace {pace} is not on-trigger, on-stop or on-declare"
            ));
        }
        for server in merged
            .mcp
            .iter()
            .filter(|s| *s != "legionctl" && !self.mcp_servers.contains(s))
        {
            out.push(format!("mcp {server} is not in mcp.servers"));
        }
        for set in &merged.capabilities {
            if let Some(why) = crate::runnables::refusal(&self.config, set) {
                out.push(format!("capability set {set}: {why}"));
            }
        }
        for file in &merged.instructions {
            if !self.instruction_path(file).is_some_and(|p| p.is_file()) {
                out.push(format!("the instruction file {file} does not exist"));
            }
        }
        for step in merged
            .route
            .iter()
            .filter(|s| !["rlcd", "acp"].contains(&s.as_str()))
        {
            out.push(format!("route step {step} is not rlcd or acp"));
        }
        if !merged.route.is_empty()
            && !merged.route.iter().any(|s| s == "acp")
            && merged.rlcd.is_none()
        {
            out.push("the route has no configured step: add acp, or configure rlcd".into());
        }
        let action = merged.rlcd.as_ref().map(|r| r.questions.get("action"));
        let actions = |o: &BTreeMap<String, String>| o.keys().map(String::as_str).eq(ACTIONS);
        if let Some(found) = action
            && !matches!(found, Some(crate::rlcd::Question::Choice { options, .. }) if actions(options))
        {
            out.push(format!(
                "rlcd.questions needs the choice action with the options {}",
                ACTIONS.join(", ")
            ));
        }
        out
    }

    fn instruction_path(&self, file: &str) -> Option<PathBuf> {
        let path = crate::config::expand_home(file).ok()?;
        Some(if path.is_absolute() {
            path
        } else {
            self.repo.as_ref()?.join(path)
        })
    }

    /// Resolve each member of `team` (none: each config as a member of its own) for `harness`; with `sessions`, each
    /// member runs as its own operator session (a main agent's run path), else in the main agent's harness.
    ///
    /// # Errors
    /// The team file cannot be found or read.
    pub fn check(
        &self,
        team: Option<(&str, &Path)>,
        harness: Harness,
        sessions: bool,
    ) -> Result<Check> {
        let mut refused = Vec::new();
        let (members, flow) = match team {
            Some((_, path)) => {
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("reading {}", path.display()))?;
                let team: Team =
                    serde_json::from_reader(json_comments::StripComments::new(text.as_bytes()))
                        .with_context(|| format!("{} is not a valid team file", path.display()))?;
                (team.members, team.flow)
            }
            None => (
                self.configs
                    .keys()
                    .map(|name| (name.clone(), AgentConfig::default()))
                    .collect(),
                Flow::default(),
            ),
        };
        let mut resolved = Vec::new();
        for (name, layer) in &members {
            let Some((mut member, _, _)) = self.member(
                name,
                layer,
                team.map(|t| t.0),
                (harness, sessions),
                &mut refused,
            ) else {
                continue;
            };
            if let Some(may) = flow.may.get(name) {
                let allowed = &member.fields["may"];
                for kind in may.iter().filter(|k| {
                    !allowed
                        .as_array()
                        .is_some_and(|a| a.contains(&Value::from(k.as_str())))
                }) {
                    refused.push(format!(
                        "{name}: flow.may widens: {} may not send {kind}",
                        member.archetype
                    ));
                }
                member.fields.insert("may".into(), may.clone().into());
            }
            resolved.push(member);
        }
        for name in flow.may.keys().filter(|n| !members.contains_key(*n)) {
            refused.push(format!("flow.may names {name}, which is not a member"));
        }
        refused.extend(flow.problems(&members));
        if team.is_some() {
            let mains = resolved
                .iter()
                .filter(|m| m.archetype == "main-agent")
                .count();
            if mains != 1 {
                refused.push(format!(
                    "the team has {mains} main-agent members; it needs one"
                ));
            }
        }
        refused.extend(self.set_problems.iter().cloned());
        let sets = self
            .config
            .capabilities
            .iter()
            .map(|l| SetState {
                name: l.set.name.clone(),
                version: l.set.version,
                layer: l.source.layer.clone(),
                file: l.source.file.clone(),
                trust: l.trust,
                powerful: l.set.powerful().iter().map(|p| (*p).to_string()).collect(),
                refusal: crate::runnables::refusal(&self.config, &l.set.name),
            })
            .collect();
        Ok(Check {
            harness,
            team: team.map(|t| t.0.to_string()),
            members: resolved,
            sets,
            refused,
            flow,
        })
    }

    /// The Codex agent file of each config that can run as a Codex subagent: name -> its TOML text. A config for
    /// Claude only is left out.
    ///
    /// # Errors
    /// A config that a Codex subagent cannot apply, with each field named.
    pub fn codex_agents(&self) -> Result<BTreeMap<String, String>> {
        let mut out = BTreeMap::new();
        let mut refused = Vec::new();
        for (name, config) in &self.configs {
            if config.harness == Some(Harness::Claude) {
                continue;
            }
            // as a member that is not the main agent: the main Codex session is the user's own
            let Some((_, file, merged)) = self.member(
                name,
                &AgentConfig::default(),
                None,
                (Harness::Codex, false),
                &mut refused,
            ) else {
                continue;
            };
            out.insert(name.clone(), self.codex_toml(name, &file, &merged));
        }
        anyhow::ensure!(
            refused.is_empty(),
            "Codex agent files: {}. Set \"harness\": \"claude\" in a config that is not for Codex",
            refused.join("; ")
        );
        Ok(out)
    }

    /// The instruction text of an operator member: its archetype file's text with the plugin folder in place of the
    /// variable, and each instruction file that its config adds.
    ///
    /// # Errors
    /// The archetype file is missing or not valid.
    pub fn instructions(&self, archetype: &str, extra: &[String]) -> Result<String> {
        let mut refused = Vec::new();
        let file = archetype_file(&self.plugin, archetype, &mut refused)
            .with_context(|| refused.join("; "))?;
        let mut text = file
            .body
            .replace(ROOT_VAR, &format!("{}/", self.plugin.display()));
        for path in extra.iter().filter_map(|e| self.instruction_path(e)) {
            let _ = write!(text, "\n\nRead and follow {}.", path.display());
        }
        Ok(text)
    }

    fn codex_toml(&self, name: &str, file: &ArchetypeFile, merged: &AgentConfig) -> String {
        let mut text = file
            .body
            .replace(ROOT_VAR, &format!("{}/", self.plugin.display()));
        if !merged.skills.is_empty() {
            let skills: Vec<String> = merged
                .skills
                .iter()
                .map(|s| format!("legionctl-{s}"))
                .collect();
            let _ = write!(text, "\n\nSkills for this agent: {}.", skills.join(", "));
        }
        for extra in &merged.instructions {
            if let Some(path) = self.instruction_path(extra) {
                let _ = write!(text, "\n\nRead and follow {}.", path.display());
            }
        }
        let mut table = toml::Table::new();
        table.insert("name".into(), name.into());
        table.insert("description".into(), file.description.clone().into());
        if let Some(model) = &merged.model {
            table.insert("model".into(), model.clone().into());
        }
        if let Some(effort) = &merged.effort {
            table.insert("model_reasoning_effort".into(), effort.clone().into());
        }
        table.insert("developer_instructions".into(), text.into());
        format!(
            "{CODEX_HEADER} from {} and agents.configs.{name}; change those, then run it again.\n{}",
            file.path.display(),
            toml::to_string(&table).unwrap_or_default()
        )
    }
}

fn is_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.values().all(is_empty),
        _ => false,
    }
}

/// Whether a Codex agent file was written by legionctl (and may be removed when its config is gone).
#[must_use]
pub fn written_by_legionctl(text: &str) -> bool {
    text.starts_with(CODEX_HEADER)
}
