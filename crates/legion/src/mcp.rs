//! MCP server over stdio. Read-only tools answer immediately; `request_action` makes the change only after a
//! human approves the polkit dialog on this machine, so an agent can propose but never silently apply.

use std::path::PathBuf;

use legion_core::{actions, agents, load, setup, snapshot::Snapshot, worktree::Store};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock as Content};
use rmcp::{ServiceExt, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

pub struct Legion;

fn ok<T: serde::Serialize>(value: &T) -> CallToolResult {
    match serde_json::to_string_pretty(value) {
        Ok(text) => CallToolResult::success(vec![Content::text(text)]),
        Err(e) => fail(e),
    }
}

fn fail(error: impl std::fmt::Display) -> CallToolResult {
    CallToolResult::error(vec![Content::text(error.to_string())])
}

#[derive(Deserialize, JsonSchema)]
pub struct ProjectArgs {
    /// Project directory whose .claude/.codex settings to include; defaults to the server's working directory.
    pub project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListArgs {
    /// Measure disk usage of each worktree (slower on large checkouts).
    #[serde(default)]
    pub sizes: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateArgs {
    /// Absolute path of the git repository to branch from.
    pub repo: String,
    /// Worktree name: letters, digits, '-', '_', '.'; becomes branch legion/NAME.
    pub name: String,
    /// Git ref to start from; defaults to the repo's HEAD.
    pub base: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct RemoveArgs {
    pub name: String,
    /// Discard uncommitted changes in the worktree.
    #[serde(default)]
    pub force: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct ActionArgs {
    /// "apply-power-policy" (value: auto|ac|battery) or "set-desktop-gpu" (value: nvidia|intel|auto).
    pub action: String,
    pub value: String,
}

#[tool_router(server_handler)]
impl Legion {
    #[tool(
        description = "Power source, Fn+Q thermal mode, firmware power limits, CPU policy, temperature, GPU wiring and load of this Lenovo Legion laptop."
    )]
    fn system_status(&self) -> CallToolResult {
        ok(&Snapshot::read())
    }

    #[tool(
        description = "Current system load level (idle, normal, busy, overloaded) with reasons from CPU/memory/IO pressure. Check before starting builds, test suites or parallel jobs."
    )]
    fn system_load(&self) -> CallToolResult {
        let l = load::read_load();
        CallToolResult::success(vec![
            Content::text(crate::brief(&l)),
            Content::text(serde_json::to_string_pretty(&l).unwrap_or_default()),
        ])
    }

    #[tool(
        description = "Read-only health checks of power policy, kernel modules, DKMS, GPU placement and conflicts (legionctl doctor)."
    )]
    fn doctor(&self) -> CallToolResult {
        actions::doctor().map_or_else(fail, |text| {
            CallToolResult::success(vec![Content::text(text)])
        })
    }

    #[tool(
        description = "Claude Code and Codex settings files (user and project): present, valid, which top-level sections. Never returns values."
    )]
    fn agent_configs(&self, Parameters(args): Parameters<ProjectArgs>) -> CallToolResult {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let project = args.project.map_or_else(
            || std::env::current_dir().unwrap_or_default(),
            PathBuf::from,
        );
        ok(&agents::config_files(&home, &project))
    }

    #[tool(description = "List agent worktrees with branch, dirty state and optional disk usage.")]
    fn worktree_list(&self, Parameters(args): Parameters<ListArgs>) -> CallToolResult {
        Store::default()
            .list(args.sizes)
            .map_or_else(fail, |list| ok(&list))
    }

    #[tool(
        description = "Create an isolated git worktree of a repository on a new branch legion/NAME, under /data/worktrees/REPO/NAME. Use one per task so parallel work never collides."
    )]
    fn worktree_create(&self, Parameters(args): Parameters<CreateArgs>) -> CallToolResult {
        Store::default()
            .create(&PathBuf::from(args.repo), &args.name, args.base.as_deref())
            .map_or_else(fail, |wt| ok(&wt))
    }

    #[tool(
        description = "Remove a worktree's checkout. Its branch is kept. Refuses when there are uncommitted changes unless force is true."
    )]
    fn worktree_remove(&self, Parameters(args): Parameters<RemoveArgs>) -> CallToolResult {
        Store::default()
            .remove(&args.name, args.force)
            .map_or_else(fail, |wt| ok(&wt))
    }

    #[tool(
        description = "Ask to change the machine: apply-power-policy (auto|ac|battery) or set-desktop-gpu (nvidia|intel|auto). A human must approve the polkit dialog on this machine; if they decline, nothing changes and an error is returned."
    )]
    fn request_action(&self, Parameters(args): Parameters<ActionArgs>) -> CallToolResult {
        actions::run_approved(&args.action, &args.value).map_or_else(fail, |text| {
            CallToolResult::success(vec![Content::text(text)])
        })
    }

    #[tool(
        description = "Check whether ctx (agent history search) and legion are set up for Claude Code and Codex: skills and MCP servers. Read-only."
    )]
    fn setup_check(&self) -> CallToolResult {
        ok(&setup::check())
    }

    #[tool(
        description = "Set up ctx and legion for Claude Code and Codex: install the ctx skill and MCP server for both, add legion's MCP server to ~/.codex/config.toml (with a backup) and link legion's skills for Codex. Changes user-level config files only."
    )]
    fn setup_apply(&self) -> CallToolResult {
        setup::apply(false).map_or_else(fail, |done| ok(&done))
    }
}

pub async fn serve() -> anyhow::Result<()> {
    Legion
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await?;
    Ok(())
}
