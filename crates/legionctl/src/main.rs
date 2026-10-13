//! `legionctl`: the CLI, TUI and MCP server of legionctl. Every command is an operation of `legionctl-operator`
//! (see `legionctl-api`); only `mcp`, `tui` and `run` are local, because they are surfaces or must run in this
//! process.

mod cli;
mod hook;
mod mcp;
mod proxy;
mod tui;

use std::os::unix::process::CommandExt;

use anyhow::{Context, Result, ensure};
use clap::{Arg, ArgAction, ArgMatches, Command};
use legionctl_api::{ClientPath, Cores, EventsIn, GateIn, Launch, RunEnvIn, RunGpu, op};

use legionctl_api::client::Client;

fn client_only() -> Vec<Command> {
    vec![
        Command::new("mcp").about("MCP server over stdio, for Claude Code, Codex and other agents"),
        Command::new("mcp-proxy")
            .about("A session agent's way to an MCP server: only the allowed tools, every call logged (tools calls)")
            .arg(Arg::new("server").long("server").required(true))
            .arg(Arg::new("allow").long("allow").action(ArgAction::Append).help("A tool the agent may call; none: all"))
            .arg(
                Arg::new("command")
                    .num_args(1..)
                    .allow_hyphen_values(true)
                    .required(true)
                    .last(true),
            ),
        Command::new("tui").about("Live dashboard"),
        Command::new("trace").about("Follow the event log: agents, people and the machine daemon, as it happens"),
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

/// The git gate: this binary is `/usr/bin/git` in a session sandbox. It asks the operator, then runs the real git
/// (mounted at `LEGIONCTL_REAL_GIT`) or refuses with the reason.
fn gate_git() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let input = GateIn {
        args: args.clone(),
        project: ClientPath(std::env::current_dir()?),
    };
    let guard = tokio::runtime::Runtime::new()?
        .block_on(Client::new("gate")?.call::<op::GatesCheck>(&input))?;
    if let Some(reason) = guard.refusal {
        eprintln!("{reason}");
        std::process::exit(1);
    }
    let real = std::env::var_os("LEGIONCTL_REAL_GIT")
        .context("LEGIONCTL_REAL_GIT is not set (outside a session?)")?;
    let err = std::process::Command::new(real).args(args).exec();
    Err(err).context("running the real git")
}

/// `legionctl trace`: print new events as they arrive (the backlog is skipped).
fn trace() -> Result<()> {
    let client = Client::new("cli")?;
    let runtime = tokio::runtime::Runtime::new()?;
    let page =
        |since: i64| runtime.block_on(client.call::<op::EventsOp>(&EventsIn { since, limit: 500 }));
    let mut last = 0;
    loop {
        let events = page(last)?;
        match events.last() {
            Some(e) => last = e.id,
            None => break,
        }
    }
    loop {
        let events = page(last)?;
        if let Some(e) = events.last() {
            last = e.id;
            println!("{}", legionctl_api::Render::text(&events));
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// `legionctl session start`: run the prepared session in this terminal, with this binary as the git gate.
fn start_session(launch: Launch) -> Result<()> {
    let exe = std::env::current_exe()?.to_string_lossy().into_owned();
    let real_git = launch.real_git.to_string_lossy().into_owned();
    let gate = [
        "--ro-bind".into(),
        exe,
        "/usr/bin/git".into(),
        "--setenv".into(),
        "LEGIONCTL_REAL_GIT".into(),
        real_git,
    ];
    let argv = [launch.scope, launch.sandbox, gate.to_vec(), launch.agent].concat();
    eprintln!(
        "legionctl: session {} (stop it with `legionctl session stop {}`)",
        launch.id, launch.id
    );
    let err = std::process::Command::new(&argv[0]).args(&argv[1..]).exec();
    Err(err).with_context(|| format!("running {}", argv[0]))
}

fn main() -> Result<()> {
    if std::env::args_os()
        .next()
        .and_then(|a| std::path::Path::new(&a).file_name().map(|n| n == "git"))
        == Some(true)
    {
        return gate_git();
    }
    let matches = cli::command(client_only()).get_matches();
    match matches.subcommand() {
        Some(("mcp", _)) => return tokio::runtime::Runtime::new()?.block_on(mcp::serve()),
        Some(("mcp-proxy", sub)) => {
            let strings = |id: &str| {
                sub.get_many::<String>(id)
                    .into_iter()
                    .flatten()
                    .cloned()
                    .collect()
            };
            let server = sub.get_one::<String>("server").cloned().unwrap_or_default();
            return tokio::runtime::Runtime::new()?.block_on(proxy::serve(
                server,
                strings("allow"),
                strings("command"),
            ));
        }
        Some(("tui", _)) => return tui::run(),
        Some(("trace", _)) => return trace(),
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
    if op.name == "session.start" {
        return start_session(serde_json::from_value(output)?);
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
