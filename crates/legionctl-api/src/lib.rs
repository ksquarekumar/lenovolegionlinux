//! Every legionctl operation, declared once in [`operations!`]. From that one list come:
//!
//! - [`OPS`]: name, permission class, help text and input JSON schema, from which the clients build the CLI and the
//!   MCP tools and the operator builds its HTTP routes;
//! - [`Engine`]: one method per operation, which `legionctl-operator` implements (a missing handler does not
//!   compile), and [`dispatch`], which routes a call by name;
//! - one marker type per operation in [`op`], for typed client calls, and [`render`] for the text a CLI prints.

mod io;
mod render;

use std::borrow::Cow;
use std::path::PathBuf;

use anyhow::Result;
use schemars::generate::SchemaSettings;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use io::*;
pub use render::Render;

/// Socket path relative to `$XDG_RUNTIME_DIR`.
pub const SOCKET: &str = "legionctl/api.sock";
/// HTTP header that says which surface made a call: cli, mcp or tui.
pub const ACTOR_HEADER: &str = "x-legionctl-actor";

/// What an operation may change, and so which gate applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    /// Reads only; no gate.
    Read,
    /// Writes in the user's workspaces and agent settings; the agent harness asks for tool permission.
    Workspace,
    /// Changes the machine; `legionctl-system` asks for polkit approval.
    Machine,
}

/// One operation as the surfaces see it.
#[derive(Debug, Clone)]
pub struct OpInfo {
    /// Dotted name: `worktree.create` is `POST /v1/ops/worktree.create`, `legionctl worktree create` and the MCP
    /// tool `worktree_create`.
    pub name: &'static str,
    /// The doc comment as one paragraph; see [`OpInfo::summary`].
    pub help: &'static str,
    pub class: Class,
    /// JSON schema of the input, with every subschema inline.
    pub input: fn() -> Value,
}

impl OpInfo {
    /// The help text without the leading space of each doc line.
    #[must_use]
    pub fn text(&self) -> &'static str {
        self.help.trim()
    }

    /// The first sentence of the help text.
    #[must_use]
    pub fn summary(&self) -> &'static str {
        let text = self.text();
        text.split_once(". ").map_or(text, |(first, _)| first)
    }
}

/// Who made a call; the operator records it with every call that is not a read.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub actor: String,
}

/// A filesystem path in an input. Clients resolve a relative path against their own working directory before they
/// send it, because the operator runs elsewhere. Its schema has `"format": "path"`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClientPath(pub PathBuf);

impl JsonSchema for ClientPath {
    fn schema_name() -> Cow<'static, str> {
        "ClientPath".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({"type": "string", "format": "path"})
    }

    fn inline_schema() -> bool {
        true
    }
}

/// A typed operation: its name and its input and output types.
pub trait Op {
    const NAME: &'static str;
    type In: Serialize + DeserializeOwned + JsonSchema;
    type Out: Serialize + DeserializeOwned + Render;
}

fn input_schema<T: JsonSchema>() -> Value {
    let schema = SchemaSettings::draft2020_12()
        .with(|s| s.inline_subschemas = true)
        .into_generator()
        .into_root_schema_for::<T>();
    schema.to_value()
}

/// A call that cannot reach an engine method.
#[derive(Debug)]
pub enum DispatchError {
    /// No operation has this name.
    Unknown(String),
    /// The input does not match the operation's schema.
    Input(serde_json::Error),
    /// The engine method failed.
    Failed(anyhow::Error),
}

macro_rules! operations {
    ($(
        $(#[doc = $doc:literal])+
        $method:ident / $Marker:ident = $name:literal, $class:ident, $in:ty => $out:ty;
    )*) => {
        /// Every operation, in help order.
        pub const OPS: &[OpInfo] = &[$(OpInfo {
            name: $name,
            help: concat!($($doc),+),
            class: Class::$class,
            input: input_schema::<$in>,
        }),*];

        /// What `legionctl-operator` implements: one method per operation.
        pub trait Engine: Sync {
            $(
                $(#[doc = $doc])+
                ///
                /// # Errors
                /// The operation fails; the message goes to the caller.
                fn $method(&self, ctx: &Ctx, input: $in) -> Result<$out>;
            )*
        }

        /// Run the operation `name` with a JSON input.
        ///
        /// # Errors
        /// See [`DispatchError`].
        pub fn dispatch(engine: &dyn Engine, ctx: &Ctx, name: &str, input: Value) -> Result<Value, DispatchError> {
            match name {
                $($name => {
                    let input: $in = serde_json::from_value(input).map_err(DispatchError::Input)?;
                    let output = engine.$method(ctx, input).map_err(DispatchError::Failed)?;
                    serde_json::to_value(output).map_err(|e| DispatchError::Failed(e.into()))
                })*
                other => Err(DispatchError::Unknown(other.to_string())),
            }
        }

        /// Text for a terminal, and whether the result is a success (a CLI exits 1 when it is not).
        ///
        /// # Errors
        /// `name` is not an operation, or `output` is not its output type.
        pub fn render(name: &str, output: Value) -> Result<(String, bool)> {
            match name {
                $($name => {
                    let output: $out = serde_json::from_value(output)?;
                    Ok((output.text(), output.success()))
                })*
                other => anyhow::bail!("unknown operation {other}"),
            }
        }

        /// Marker types for typed calls: `client.call::<op::Status>(&input)`.
        pub mod op {
            use super::*;
            $(
                $(#[doc = $doc])+
                pub struct $Marker;
                impl Op for $Marker {
                    const NAME: &'static str = $name;
                    type In = $in;
                    type Out = $out;
                }
            )*
        }
    };
}

operations! {
    /// Power source, thermal mode, firmware limits, CPU policy, temperature, GPU wiring and load.
    status / Status = "status", Read, StatusIn => Snapshot;
    /// Load level (idle, normal, busy, overloaded) and why, from CPU, memory and I/O pressure. Check it before
    /// builds, test suites or parallel jobs.
    load / Load = "load", Read, Empty => LoadState;
    /// Read-only health checks: power policy drift, conflicting units, packages and modules, GPU wiring.
    doctor / Doctor = "doctor", Read, ProfileIn => Findings;
    /// Known and planned capability coverage from the profile.
    capabilities / Capabilities = "capabilities", Read, ProfileIn => CapabilityList;
    /// Check the profile's structure and ac/battery consistency (no hardware access).
    validate / Validate = "validate", Read, ProfileIn => Validation;
    /// GPU wiring: which GPU drives each display and which GPU draws the desktop.
    gpu_status / GpuStatus = "gpu.status", Read, Empty => GpuState;
    /// Environment and CPU list that run a program on a chosen GPU and core class (`legionctl run` uses it).
    gpu_run_env / GpuRunEnv = "gpu.run-env", Read, RunEnvIn => RunEnv;
    /// Apply the power policy for ac, battery or the current power source (auto). A human approves it in the
    /// polkit dialog. Clears an Fn+Q choice.
    power_apply / PowerApply = "power.apply", Machine, PowerApplyIn => Changes;
    /// Choose which GPU draws the desktop (nvidia, intel, auto); Mutter reads it at the next login. A human
    /// approves it in the polkit dialog.
    gpu_primary / GpuPrimary = "gpu.primary", Machine, GpuPrimaryIn => Changes;
    /// Disable conflicting units from earlier Legion tooling and optionally unload modules; print the command that
    /// removes conflicting packages. A human approves it in the polkit dialog.
    cleanup / Cleanup = "cleanup", Machine, CleanupIn => Changes;
    /// Claude Code and Codex settings files (user and project): present, valid, which top-level sections.
    agents / Agents = "agents", Read, ProjectIn => ConfigFiles;
    /// Agent worktrees with branch, dirty state and optional disk usage.
    worktree_list / WorktreeList = "worktree.list", Read, WorktreeListIn => Worktrees;
    /// Create a worktree of a repository on a new branch legionctl/NAME, one per task.
    worktree_create / WorktreeCreate = "worktree.create", Workspace, WorktreeCreateIn => Worktree;
    /// Remove a worktree's checkout; its branch is kept. Refuses uncommitted changes unless force is set.
    worktree_rm / WorktreeRm = "worktree.rm", Workspace, WorktreeRmIn => Worktree;
    /// Whether ctx and legionctl are set up for Claude Code and Codex (skills, MCP servers).
    setup_check / SetupCheck = "setup.check", Read, Empty => SetupReport;
    /// Set up ctx and legionctl for Claude Code and Codex. Changes user-level config files only, with backups.
    setup_apply / SetupApply = "setup.apply", Workspace, SetupApplyIn => Changes;
    /// Reminders that hooks inject into agent sessions, with on/off state and when they fire.
    reminders_list / RemindersList = "reminders.list", Read, ProjectIn => Reminders;
    /// Turn a reminder on.
    reminders_on / RemindersOn = "reminders.on", Workspace, IdIn => Reminders;
    /// Turn a reminder off.
    reminders_off / RemindersOff = "reminders.off", Workspace, IdIn => Reminders;
    /// Add your own reminder (one line, 300 characters max).
    reminders_add / RemindersAdd = "reminders.add", Workspace, ReminderAddIn => Reminders;
    /// Remove a reminder you added.
    reminders_rm / RemindersRm = "reminders.rm", Workspace, IdIn => Reminders;
    /// Enabled reminders for a hook event, one per line.
    reminders_emit / RemindersEmit = "reminders.emit", Read, EventIn => Text;
    /// Hook actions (built-in and your commands) and what runs, in order, for each event.
    hooks_list / HooksList = "hooks.list", Read, ProjectIn => HookTable;
    /// Run an action when an event fires, after the ones already bound.
    hooks_bind / HooksBind = "hooks.bind", Workspace, HookBindIn => HookTable;
    /// Stop running an action for an event.
    hooks_unbind / HooksUnbind = "hooks.unbind", Workspace, HookBindIn => HookTable;
    /// Define your own action: a one-line shell command whose output goes to the agent.
    hooks_add / HooksAdd = "hooks.add", Workspace, HookAddIn => HookTable;
    /// Remove an action you defined, and its bindings.
    hooks_rm / HooksRm = "hooks.rm", Workspace, IdIn => HookTable;
    /// Run everything bound to an event and return the combined output (the agent plugin calls this).
    hooks_run / HooksRun = "hooks.run", Workspace, EventIn => Text;
    /// legionctl skills: where each agent has them and whether the reminder that keeps each in force is on.
    skills / Skills = "skills", Read, ProjectIn => SkillList;
    /// The merged config (defaults, /etc/legionctl, ~/.config/legionctl, the repository's .legionctl) and the files
    /// or keys that were not used.
    config_show / ConfigShow = "config.show", Read, ProjectIn => ConfigView;
    /// Which file set a key, the values it overrode, and whether /etc/legionctl locks it.
    config_explain / ConfigExplain = "config.explain", Read, ExplainIn => Explanation;
    /// The config layers in merge order, their files, and the locked keys.
    config_layers / ConfigLayers = "config.layers", Read, ProjectIn => LayerList;
}
