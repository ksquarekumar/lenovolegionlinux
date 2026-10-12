//! `legion`: operator CLI, TUI and MCP server for this machine. Every subcommand goes through legion-core, so the
//! terminal, the TUI and agents (over MCP) see the same state and hit the same guard rails.

mod mcp;
mod tui;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use legion_core::{actions, agents, load, setup, snapshot::Snapshot, worktree::Store};

#[derive(Parser)]
#[command(
    version,
    about = "Operate this Legion: state, load, worktrees, agent configs and approved actions"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Power, firmware, GPU and load state
    Status {
        #[arg(long)]
        json: bool,
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
    Doctor,
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
    /// Set up ctx and legion for Claude Code and Codex (skills, MCP servers)
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
    /// MCP server over stdio, for Claude Code, Codex and other agents
    Mcp,
    /// Live dashboard
    Tui,
}

#[derive(Subcommand)]
enum WorktreeCommand {
    /// Create a worktree of REPO on a new branch legion/NAME
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

pub fn human_bytes(bytes: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", units[unit])
    }
}

/// One line an agent can act on: the level, why, and what to do about it.
pub fn brief(load: &load::Load) -> String {
    let advice = match load.level {
        load::Level::Overloaded => " Avoid builds, test suites and parallel jobs until it settles.",
        load::Level::Busy => " Prefer light commands; limit parallelism (e.g. -j4).",
        _ => "",
    };
    let why = if load.reasons.is_empty() {
        String::new()
    } else {
        format!(" ({})", load.reasons.join(", "))
    };
    format!(
        "System load: {:?}{why}; load {:.1} on {} CPUs, {} of {} free.{advice}",
        load.level,
        load.load1,
        load.cpus,
        human_bytes(load.mem_available_mib * 1024 * 1024),
        human_bytes(load.mem_total_mib * 1024 * 1024)
    )
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Status { json: true } => print_json(&Snapshot::read())?,
        Command::Status { json: false } => {
            let s = Snapshot::read();
            println!(
                "power: {}  mode: {}{}",
                s.power_source,
                s.platform_profile.as_deref().unwrap_or("?"),
                s.fn_q_override
                    .as_ref()
                    .map_or(String::new(), |o| format!(" (Fn+Q: {o})"))
            );
            println!(
                "cpu: governors {:?}  EPP {:?}",
                s.governors, s.energy_performance_preference
            );
            println!(
                "limits: RAPL {:.0}/{:.0} W  firmware {:?}",
                s.rapl_sustained_w.unwrap_or(0.0),
                s.rapl_burst_w.unwrap_or(0.0),
                s.firmware_limits
            );
            println!(
                "temp: {}  gpu: {} (desktop on {})",
                s.package_temp_c
                    .map_or("?".into(), |t| format!("{t:.0} °C")),
                s.gpu_mode,
                s.desktop_gpu
            );
            for d in &s.displays {
                println!("  {} on {} ({})", d.connector, d.card, d.driver);
            }
            println!("{}", brief(&s.load));
        }
        Command::Load {
            json,
            brief: short,
            only_busy,
        } => {
            let l = load::read_load();
            if json {
                print_json(&l)?;
            } else if !(short && only_busy && l.level < load::Level::Busy) {
                println!("{}", brief(&l));
            }
        }
        Command::Doctor => print!("{}", actions::doctor()?),
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
                        let size = w.size_bytes.map_or("-".into(), human_bytes);
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
                        "legion setup: {} need attention ({}). Run /legion:setup or `legion setup`.",
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
        Command::Mcp => tokio::runtime::Runtime::new()?.block_on(mcp::serve())?,
        Command::Tui => tui::run()?,
    }
    Ok(())
}
