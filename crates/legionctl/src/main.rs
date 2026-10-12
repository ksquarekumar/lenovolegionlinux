//! `legionctl` (binary `legion` until the switch in docs/design/15-layout.md): operator CLI, TUI and MCP server for this machine. Every subcommand goes through legionctl-core, so the
//! terminal, the TUI and agents (over MCP) see the same state and hit the same guard rails.

mod machine;
mod mcp;
mod tui;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use legionctl_core::reminders::{self, Event};
use legionctl_core::{actions, agents, setup, worktree::Store};
use legionctl_core::{db::Db, hooks::Hooks, skills};
use legionctl_machine::load;
use legionctl_machine::snapshot::Detail;
use legionctl_machine::sysroot::SysRoot;

#[derive(Parser)]
#[command(
    version,
    about = "Operate this Legion: state, load, worktrees, agent configs and approved actions"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum SourceArg {
    Ac,
    Battery,
    Auto,
}

impl SourceArg {
    fn source(self) -> Option<legionctl_machine::policy::Source> {
        use legionctl_machine::policy::Source;
        match self {
            SourceArg::Ac => Some(Source::Ac),
            SourceArg::Battery => Some(Source::Battery),
            SourceArg::Auto => None,
        }
    }
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum GpuAction {
    Status,
    Primary,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum GpuChoice {
    Nvidia,
    Intel,
    Auto,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum RunGpu {
    Nvidia,
    Intel,
    Default,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Cores {
    P,
    E,
    All,
}

#[derive(clap::Args)]
struct PolicyArgs {
    /// Profile (JSONC); default: the installed one
    #[arg(long)]
    profile: Option<PathBuf>,
    /// Print what would change; change nothing
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    quiet: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Apply the power policy for ac, battery, or the current power source (auto)
    Apply {
        #[arg(value_enum)]
        mode: SourceArg,
        #[command(flatten)]
        args: PolicyArgs,
        /// Apply even if the machine's DMI does not match the profile
        #[arg(long)]
        force: bool,
        /// Keep an Fn+Q choice (used after resume)
        #[arg(long)]
        keep_override: bool,
    },
    /// Restore firmware mode, PPD, governor and EPP only
    Reassert {
        #[arg(value_enum)]
        mode: SourceArg,
        #[command(flatten)]
        args: PolicyArgs,
        #[arg(long)]
        force: bool,
    },
    /// React to a firmware platform_profile change (Fn+Q)
    Follow {
        #[command(flatten)]
        args: PolicyArgs,
    },
    /// Show GPU wiring, or choose which GPU draws the desktop (at the next login)
    Gpu {
        #[arg(value_enum, default_value = "status")]
        action: GpuAction,
        #[arg(value_enum)]
        choice: Option<GpuChoice>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Run a program on a chosen GPU, core class and priority
    Run {
        #[arg(long, value_enum, default_value = "default")]
        gpu: RunGpu,
        #[arg(long, value_enum, default_value = "all")]
        cores: Cores,
        /// Absolute niceness for the program
        #[arg(long, allow_hyphen_values = true)]
        nice: Option<i32>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        cmd: Vec<String>,
    },
    /// Disable conflicting units from earlier Legion tooling; optionally unload modules and remove packages
    Cleanup {
        #[command(flatten)]
        args: PolicyArgs,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        unload_modules: bool,
        #[arg(long)]
        remove_packages: bool,
    },
    /// Known and planned capability coverage from the profile
    Capabilities {
        #[arg(long)]
        profile: Option<PathBuf>,
    },
    /// Check the profile's structure and ac/battery consistency (no hardware access)
    Validate {
        #[arg(long)]
        profile: Option<PathBuf>,
    },
    /// Power, firmware, GPU and load state
    Status {
        #[arg(long)]
        json: bool,
        /// Profile (JSONC); default: the installed one
        #[arg(long)]
        profile: Option<PathBuf>,
    },
    /// System load level and why
    Load {
        #[arg(long)]
        json: bool,
        /// One line for agent hooks
        #[arg(long)]
        brief: bool,
        /// With --brief: print nothing unless the machine is busy or overloaded
        #[arg(long)]
        only_busy: bool,
    },
    /// Read-only health checks (legionctl doctor)
    Doctor {
        /// Profile (JSONC); default: the installed one
        #[arg(long)]
        profile: Option<PathBuf>,
    },
    /// Claude Code and Codex settings files: present, valid, which sections
    Agents {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Isolated git worktrees for agents and experiments
    #[command(subcommand)]
    Worktree(WorktreeCommand),
    /// Change the machine; each action asks a human to approve in the polkit dialog
    Act {
        /// apply-power-policy or set-desktop-gpu
        action: String,
        /// auto|ac|battery for apply-power-policy, nvidia|intel|auto for set-desktop-gpu
        value: String,
    },
    /// Set up ctx and legionctl for Claude Code and Codex (skills, MCP servers)
    Setup {
        /// Only report what is missing; change nothing
        #[arg(long)]
        check: bool,
        /// With --check: one line, and only when something needs attention (for agent hooks)
        #[arg(long)]
        brief: bool,
        /// Install ctx with its signed installer if it is missing
        #[arg(long)]
        install_ctx: bool,
        #[arg(long)]
        json: bool,
    },
    /// Reminders that agent hooks inject (skills to follow, working habits)
    Reminders {
        #[command(subcommand)]
        command: Option<ReminderCommand>,
    },
    /// What agent hooks run: bind built-in or your own actions to hook events
    Hooks {
        #[command(subcommand)]
        command: Option<HookCommand>,
    },
    /// legionctl skills: where each agent has them and whether their reminder is on
    Skills {
        #[arg(long)]
        json: bool,
    },
    /// MCP server over stdio, for Claude Code, Codex and other agents
    Mcp,
    /// Live dashboard
    Tui,
}

#[derive(Subcommand)]
enum ReminderCommand {
    /// Show all reminders and whether they are on
    List {
        #[arg(long)]
        json: bool,
    },
    /// Turn a reminder on
    On { id: String },
    /// Turn a reminder off
    Off { id: String },
    /// Add your own reminder (one line, 300 characters max)
    Add {
        id: String,
        text: String,
        /// When to inject it: session-start (default) or prompt (every turn; keep it short)
        #[arg(long, value_enum, default_value = "session-start")]
        event: EventArg,
    },
    /// Remove a reminder you added
    Rm { id: String },
    /// Print enabled reminders for a hook event (used by the agent plugin)
    Emit {
        #[arg(long, value_enum)]
        event: EventArg,
    },
}

#[derive(Subcommand)]
enum HookCommand {
    /// Show actions and what is bound to each event
    List {
        #[arg(long)]
        json: bool,
    },
    /// Run an action when an event fires (after the ones already bound)
    Bind {
        #[arg(value_enum)]
        event: EventArg,
        action: String,
    },
    /// Stop running an action for an event
    Unbind {
        #[arg(value_enum)]
        event: EventArg,
        action: String,
    },
    /// Define your own action: a one-line shell command whose output goes to the agent
    Add {
        id: String,
        command: String,
        /// Seconds before it is stopped (1-30); the agent waits for hooks
        #[arg(long, default_value_t = 5)]
        timeout: u32,
    },
    /// Remove an action you defined, and its bindings
    Rm { id: String },
    /// Run everything bound to an event (the agent plugin calls this)
    Run {
        #[arg(long, value_enum)]
        event: EventArg,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum EventArg {
    SessionStart,
    Prompt,
}

impl From<EventArg> for Event {
    fn from(e: EventArg) -> Self {
        match e {
            EventArg::SessionStart => Event::SessionStart,
            EventArg::Prompt => Event::Prompt,
        }
    }
}

#[derive(Subcommand)]
enum WorktreeCommand {
    /// Create a worktree of REPO on a new branch legionctl/NAME
    Create {
        name: String,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Start from this ref instead of HEAD
        #[arg(long)]
        base: Option<String>,
    },
    /// List worktrees with size and dirty state
    List {
        #[arg(long)]
        json: bool,
    },
    /// Remove a worktree (its branch is kept)
    Rm {
        name: String,
        /// Discard uncommitted changes
        #[arg(long)]
        force: bool,
    },
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Apply {
            mode,
            args,
            force,
            keep_override,
        } => machine::apply(
            machine::Run {
                profile: args.profile.as_deref(),
                dry_run: args.dry_run,
                quiet: args.quiet,
            },
            mode.source(),
            legionctl_machine::policy::Options {
                force,
                reassert: false,
                keep_override,
            },
        )?,
        Command::Reassert { mode, args, force } => machine::apply(
            machine::Run {
                profile: args.profile.as_deref(),
                dry_run: args.dry_run,
                quiet: args.quiet,
            },
            mode.source(),
            legionctl_machine::policy::Options {
                force,
                reassert: true,
                keep_override: false,
            },
        )?,
        Command::Follow { args } => machine::follow(machine::Run {
            profile: args.profile.as_deref(),
            dry_run: args.dry_run,
            quiet: args.quiet,
        })?,
        Command::Gpu {
            action,
            choice,
            dry_run,
        } => match (action, choice) {
            (GpuAction::Status, _) => machine::gpu_status(),
            (GpuAction::Primary, None) => anyhow::bail!("gpu primary needs nvidia, intel or auto"),
            (GpuAction::Primary, Some(c)) => machine::gpu_primary(
                match c {
                    GpuChoice::Nvidia => "nvidia",
                    GpuChoice::Intel => "intel",
                    GpuChoice::Auto => "auto",
                },
                dry_run,
            )?,
        },
        Command::Run {
            gpu,
            cores,
            nice,
            cmd,
        } => {
            let cmd: Vec<String> = cmd.into_iter().skip_while(|a| a == "--").collect();
            machine::run(
                match gpu {
                    RunGpu::Nvidia => "nvidia",
                    RunGpu::Intel => "intel",
                    RunGpu::Default => "default",
                },
                match cores {
                    Cores::P => "p",
                    Cores::E => "e",
                    Cores::All => "all",
                },
                nice,
                &cmd,
            )?
        }
        Command::Cleanup {
            args,
            force,
            unload_modules,
            remove_packages,
        } => machine::cleanup(
            machine::Run {
                profile: args.profile.as_deref(),
                dry_run: args.dry_run,
                quiet: args.quiet,
            },
            legionctl_machine::cleanup::Options {
                force,
                unload_modules,
                remove_packages,
            },
        )?,
        Command::Capabilities { profile } => machine::capabilities(profile.as_deref())?,
        Command::Validate { profile } => {
            if !machine::validate(profile.as_deref())? {
                std::process::exit(1);
            }
        }
        Command::Status { json, profile } => {
            let s = machine::snapshot(profile.as_deref(), Detail::Full)?;
            if json {
                print_json(&s)?;
            } else {
                machine::print_status(&s);
            }
        }
        Command::Load {
            json,
            brief: short,
            only_busy,
        } => {
            let l = load::read_load(&SysRoot::from_env());
            if json {
                print_json(&l)?;
            } else if !(short && only_busy && l.level < load::Level::Busy) {
                println!("{}", load::brief(&l));
            }
        }
        Command::Doctor { profile } => {
            let (clean, report) = machine::doctor(profile.as_deref())?;
            print!("{report}");
            if !clean {
                std::process::exit(1);
            }
        }
        Command::Agents { project, json } => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            let files = agents::config_files(&home, &project);
            if json {
                print_json(&files)?;
            } else {
                for f in files {
                    let state = match (f.exists, f.valid) {
                        (false, _) => "missing".to_string(),
                        (true, Some(true)) => format!("valid [{}]", f.sections.join(", ")),
                        (true, _) => format!("INVALID: {}", f.error.unwrap_or_default()),
                    };
                    println!(
                        "{:<7} {:<14} {}  {}",
                        f.agent,
                        f.scope,
                        f.path.display(),
                        state
                    );
                }
            }
        }
        Command::Worktree(cmd) => {
            let store = Store::default();
            match cmd {
                WorktreeCommand::Create { name, repo, base } => {
                    let wt = store.create(&repo, &name, base.as_deref())?;
                    println!("created {} on branch {}", wt.path.display(), wt.branch);
                }
                WorktreeCommand::List { json: true } => print_json(&store.list(true)?)?,
                WorktreeCommand::List { json: false } => {
                    for w in store.list(true)? {
                        let size = w
                            .size_bytes
                            .map_or("-".into(), legionctl_machine::human_bytes);
                        let state = if !w.exists {
                            "missing"
                        } else if w.dirty == Some(true) {
                            "dirty"
                        } else {
                            "clean"
                        };
                        println!(
                            "{:<20} {:>10}  {:<7} {}",
                            w.worktree.name,
                            size,
                            state,
                            w.worktree.path.display()
                        );
                    }
                }
                WorktreeCommand::Rm { name, force } => {
                    let wt = store.remove(&name, force)?;
                    println!("removed {} (branch {} kept)", wt.path.display(), wt.branch);
                }
            }
        }
        Command::Act { action, value } => {
            println!(
                "requesting approval for: {}",
                actions::approved_command(&action, &value)?.join(" ")
            );
            print!("{}", actions::run_approved(&action, &value)?);
        }
        Command::Setup {
            check: true,
            brief,
            json,
            ..
        } => {
            let report = setup::check();
            let todo = report.needs_attention();
            if json {
                print_json(&report)?;
            } else if brief {
                if !todo.is_empty() {
                    let names: Vec<String> = todo
                        .iter()
                        .map(|i| format!("{} for {}", i.what, i.agent))
                        .collect();
                    println!(
                        "legionctl setup: {} need attention ({}). Run /legionctl:setup or `legion setup`.",
                        todo.len(),
                        names.join(", ")
                    );
                }
            } else {
                println!(
                    "ctx: {}",
                    report.ctx_version.as_deref().unwrap_or("not installed")
                );
                for i in &report.items {
                    println!("{:<26} {:<12} {:<14} {}", i.what, i.agent, i.status, i.path);
                }
            }
        }
        Command::Setup {
            check: false,
            install_ctx,
            ..
        } => {
            for line in setup::apply(install_ctx)? {
                println!("{line}");
            }
            let left = setup::check();
            let todo = left.needs_attention();
            println!(
                "{}",
                if todo.is_empty() {
                    "all set up".to_string()
                } else {
                    format!(
                        "{} items still need attention; run `legion setup --check`",
                        todo.len()
                    )
                }
            );
        }
        Command::Reminders { command } => {
            let store = reminders::Store::default();
            match command.unwrap_or(ReminderCommand::List { json: false }) {
                ReminderCommand::List { json: true } => print_json(&store.list()?)?,
                ReminderCommand::List { json: false } => {
                    for r in store.list()? {
                        let kind = if r.builtin { "built-in" } else { "custom" };
                        println!(
                            "{:<3} {:<20} {:<14} {:<9} {}",
                            if r.enabled { "on" } else { "off" },
                            r.id,
                            format!("{:?}", r.event),
                            kind,
                            r.text
                        );
                    }
                }
                ReminderCommand::On { id } => {
                    store.set_enabled(&id, true)?;
                    println!("{id}: on");
                }
                ReminderCommand::Off { id } => {
                    store.set_enabled(&id, false)?;
                    println!("{id}: off");
                }
                ReminderCommand::Add { id, text, event } => {
                    store.add(&id, &text, event.into())?;
                    println!("{id}: added and on");
                }
                ReminderCommand::Rm { id } => {
                    store.remove(&id)?;
                    println!("{id}: removed");
                }
                ReminderCommand::Emit { event } => {
                    let text = store.emit(event.into())?;
                    if !text.is_empty() {
                        println!("{text}");
                    }
                }
            }
        }
        Command::Hooks { command } => {
            let db = Db::open_default()?;
            let hooks = Hooks {
                db: &db,
                reminders: reminders::Store::default(),
                actor: "cli",
            };
            match command.unwrap_or(HookCommand::List { json: false }) {
                HookCommand::List { json: true } => print_json(
                    &serde_json::json!({"actions": hooks.actions()?, "bindings": hooks.bindings()?}),
                )?,
                HookCommand::List { json: false } => {
                    println!("bindings (in order):");
                    for b in hooks.bindings()? {
                        println!("  {:<14} {}", format!("{:?}", b.event), b.action);
                    }
                    println!("actions:");
                    for a in hooks.actions()? {
                        let kind = if a.builtin {
                            "built-in".to_string()
                        } else {
                            format!("command, {}s", a.timeout_s)
                        };
                        println!("  {:<16} {:<14} {}", a.id, kind, a.detail);
                    }
                }
                HookCommand::Bind { event, action } => {
                    hooks.bind(event.into(), &action)?;
                    println!("{action}: bound");
                }
                HookCommand::Unbind { event, action } => {
                    hooks.unbind(event.into(), &action)?;
                    println!("{action}: unbound");
                }
                HookCommand::Add {
                    id,
                    command,
                    timeout,
                } => {
                    hooks.add_command(&id, &command, timeout)?;
                    println!("{id}: defined; bind it with `legion hooks bind <event> {id}`");
                }
                HookCommand::Rm { id } => {
                    hooks.remove_action(&id)?;
                    println!("{id}: removed");
                }
                HookCommand::Run { event } => {
                    let text = hooks.run(event.into())?;
                    if !text.is_empty() {
                        println!("{text}");
                    }
                }
            }
        }
        Command::Skills { json } => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            let list = skills::inventory(&home, &reminders::Store::default())?;
            if json {
                print_json(&list)?;
            } else {
                for s in list {
                    let reminder = match (&s.reminder, s.reminder_on) {
                        (Some(id), Some(true)) => format!("reminder {id}: on"),
                        (Some(id), _) => format!("reminder {id}: off"),
                        _ => "no reminder".into(),
                    };
                    println!("legionctl:{}  ({reminder})", s.name);
                    println!(
                        "  claude: {}",
                        s.claude
                            .map_or("plugin not installed".into(), |p| p.display().to_string())
                    );
                    println!(
                        "  codex:  {} ({})",
                        s.codex.display(),
                        if s.codex_linked {
                            "linked"
                        } else {
                            "missing; run `legion setup`"
                        }
                    );
                }
            }
        }
        Command::Mcp => tokio::runtime::Runtime::new()?.block_on(mcp::serve())?,
        Command::Tui => tui::run()?,
    }
    Ok(())
}
