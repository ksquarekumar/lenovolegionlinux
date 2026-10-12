//! `legionctl`: the CLI, TUI and MCP server of legionctl. Every command is an operation of `legionctl-operator`
//! (see `legionctl-api`); only `mcp`, `tui` and `run` are local, because they are surfaces or must run in this
//! process.

mod cli;
mod client;
mod hook;
mod mcp;
mod tui;

use std::os::unix::process::CommandExt;

use anyhow::{Context, Result, ensure};
use clap::{Arg, ArgAction, ArgMatches, Command};
use legionctl_api::{Cores, RunEnvIn, RunGpu, op};

use crate::client::Client;

fn client_only() -> Vec<Command> {
    vec![
        Command::new("mcp").about("MCP server over stdio, for Claude Code, Codex and other agents"),
        Command::new("tui").about("Live dashboard"),
        Command::new("hook")
            .about("Claude Code hook adapter: reads the hook's JSON from stdin, answers in Claude Code's hook protocol")
            .arg(Arg::new("event").required(true).value_parser(hook::EVENTS)),
        Command::new("run")
            .about("Run a program on a chosen GPU, core class and priority")
            .arg(
                Arg::new("gpu")
                    .long("gpu")
                    .value_parser(["nvidia", "intel", "default"])
                    .default_value("default"),
            )
            .arg(
                Arg::new("cores")
                    .long("cores")
                    .value_parser(["p", "e", "all"])
                    .default_value("all")
                    .help("Core class: p (performance), e (efficiency) or all"),
            )
            .arg(
                Arg::new("nice")
                    .long("nice")
                    .allow_hyphen_values(true)
                    .value_parser(clap::value_parser!(i32))
                    .help("Absolute niceness for the program"),
            )
            .arg(
                Arg::new("cmd")
                    .num_args(1..)
                    .trailing_var_arg(true)
                    .allow_hyphen_values(true)
                    .required(true)
                    .action(ArgAction::Append),
            ),
    ]
}

/// Niceness of this process, from /proc/self/stat (field 19).
fn current_nice() -> i32 {
    std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|s| {
            s.rsplit_once(')')?
                .1
                .split_whitespace()
                .nth(16)?
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

/// `legionctl run`: replace this process with the program, with the GPU environment and CPU list from the
/// operator, at an absolute niceness.
fn run(matches: &ArgMatches) -> Result<()> {
    let choice = |key: &str| matches.get_one::<String>(key).map_or("", String::as_str);
    let input = RunEnvIn {
        gpu: match choice("gpu") {
            "nvidia" => RunGpu::Nvidia,
            "intel" => RunGpu::Intel,
            _ => RunGpu::Default,
        },
        cores: match choice("cores") {
            "p" => Cores::P,
            "e" => Cores::E,
            _ => Cores::All,
        },
    };
    let env = tokio::runtime::Runtime::new()?
        .block_on(Client::new("cli")?.call::<op::GpuRunEnv>(&input))?;
    let mut argv: Vec<String> = matches
        .get_many::<String>("cmd")
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    ensure!(
        !argv.is_empty(),
        "usage: legionctl run [--gpu nvidia|intel] [--cores p|e] [--nice N] -- command ..."
    );
    if let Some(n) = matches.get_one::<i32>("nice") {
        // `nice -n` is relative to the caller's niceness; make --nice the absolute value the program gets
        argv = [
            vec!["nice".into(), "-n".into(), (n - current_nice()).to_string()],
            argv,
        ]
        .concat();
    }
    if let Some(cpus) = env.cpus {
        // a transient scope pins the program and everything it starts; keep `$` in the command literal
        let scope = [
            "systemd-run",
            "--user",
            "--scope",
            "--quiet",
            "--expand-environment=no",
            "-p",
        ];
        argv = [
            scope.iter().map(ToString::to_string).collect(),
            vec![format!("AllowedCPUs={cpus}"), "--".into()],
            argv,
        ]
        .concat();
    }
    let err = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .envs(env.env)
        .exec();
    Err(err).with_context(|| format!("running {}", argv[0]))
}

fn main() -> Result<()> {
    let matches = cli::command(client_only()).get_matches();
    match matches.subcommand() {
        Some(("mcp", _)) => return tokio::runtime::Runtime::new()?.block_on(mcp::serve()),
        Some(("tui", _)) => return tui::run(),
        Some(("run", sub)) => return run(sub),
        Some(("hook", sub)) => {
            return hook::run(sub.get_one::<String>("event").map_or("", String::as_str));
        }
        _ => {}
    }
    let (op, input) = cli::operation(&matches)?.context("not an operation")?;
    let output = tokio::runtime::Runtime::new()?
        .block_on(Client::new("cli")?.call_value(op.name, &input))?;
    if matches.get_flag("json") {
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }
    let (text, success) = legionctl_api::render(op.name, output)?;
    if !text.is_empty() {
        println!("{text}");
    }
    if !success {
        std::process::exit(1);
    }
    Ok(())
}
