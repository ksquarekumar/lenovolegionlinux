//! `legionctl`: the CLI, TUI and MCP server of legionctl. Every command is an operation of `legionctl-operator`
//! (see `legionctl-api`); only `mcp`, `tui` and `run` are local, because they are surfaces or must run in this
//! process.

mod cli;
mod hook;
mod mcp;
mod proxy;
mod tui;
mod vm;
mod winvm;

use std::os::unix::process::CommandExt;

use anyhow::{Context, Result, ensure};
use clap::{Arg, ArgAction, ArgMatches, Command};
use legionctl_api::{ClientPath, Cores, GateIn, Launch, RunEnvIn, RunGpu, op};

use legionctl_api::client::Client;

fn client_only() -> Vec<Command> {
    vec![
        Command::new("mcp").about("MCP server over stdio, for Claude Code, Codex and other agents"),
        Command::new("mcp-proxy")
            .about("A session agent's way to an MCP server: only the allowed tools, every call logged (tools calls)")
            .arg(Arg::new("server").long("server").required(true))
            .arg(Arg::new("allow").long("allow").action(ArgAction::Append).help("A tool the agent may call; none: all"))
            .arg(
                Arg::new("cache")
                    .long("cache")
                    .action(ArgAction::Append)
                    .help("tool=ttl[:workspace]: reuse the tool's result for ttl seconds"),
            )
            .arg(Arg::new("no-network").long("no-network").action(ArgAction::SetTrue).help("Run the server without network"))
            .arg(Arg::new("url").long("url").help("A remote server (streamable HTTP) instead of a command"))
            .arg(Arg::new("header").long("header").action(ArgAction::Append).help("name=value for the remote server"))
            .arg(
                Arg::new("command")
                    .num_args(1..)
                    .allow_hyphen_values(true)
                    .required_unless_present("url")
                    .last(true),
            ),
        Command::new("tui").about("Live dashboard"),
        // a session in a VM (W5): the host's runner and relay, and the guest's boot service; legionctl starts them
        Command::new("vm")
            .hide(true)
            .arg(Arg::new("spec").required(true))
            .arg(Arg::new("agent").num_args(1..).required(true).last(true)),
        Command::new("relay").hide(true).arg(Arg::new("socket").required(true)),
        Command::new("forward")
            .hide(true)
            .arg(Arg::new("port").required(true).value_parser(clap::value_parser!(u16)))
            .arg(Arg::new("socket").required(true))
            .arg(Arg::new("agent").num_args(1..).required(true).last(true)),
        Command::new("guest").hide(true),
        Command::new("windows-vm").hide(true),
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

/// `legionctl trace`: print new events as they arrive (the backlog is skipped), from the operator's event stream.
fn trace() -> Result<()> {
    let client = Client::new("cli")?;
    tokio::runtime::Runtime::new()?.block_on(async {
        client
            .follow("now")
            .await?
            .each(
                |event| match serde_json::from_value::<legionctl_api::EventRow>(event) {
                    Ok(row) => println!("{}", legionctl_api::Render::text(&vec![row])),
                    Err(e) => eprintln!("legionctl: an event that does not parse: {e}"),
                },
            )
            .await
    })
}

/// `legionctl session run`: the run's answer is shown as it comes (its `session.output` events, under a tag of this
/// call) when the event stream is open to this caller; returns the result and the text already shown.
async fn run_streamed(
    client: &Client,
    mut input: serde_json::Value,
) -> Result<(serde_json::Value, String)> {
    let tag = legionctl_api::random_token()?;
    input["tag"] = tag.clone().into();
    let mut shown = String::new();
    // the stream starts before the run, so it misses none of its events
    let Ok(events) = client.follow("now").await else {
        return Ok((client.call_value("session.run", &input).await?, shown));
    };
    let output = {
        let follow = events.each(|event| {
            let payload = &event["payload"];
            if event["kind"] == "session.output"
                && payload["tag"] == tag.as_str()
                && let Some(text) = payload["text"].as_str()
            {
                print!("{text}");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                shown.push_str(text);
            }
        });
        tokio::pin!(follow);
        tokio::select! {
            output = client.call_value("session.run", &input) => output?,
            ended = &mut follow => {
                ended?;
                anyhow::bail!("the operator's event stream ended before the run");
            }
        }
    };
    Ok((output, shown))
}

/// `legionctl session start`: run the prepared session in this terminal, with this binary as the git gate on the
/// host (a container's image and a VM's disk have their own) and as a VM's runner.
fn start_session(mut launch: Launch) -> Result<()> {
    let exe = std::env::current_exe()?.to_string_lossy().into_owned();
    let mut gate = Vec::new();
    match launch.inside {
        legionctl_api::Inside::Host => gate.extend([
            "--ro-bind".into(),
            exe,
            "/usr/bin/git".into(),
            "--setenv".into(),
            "LEGIONCTL_REAL_GIT".into(),
            launch.real_git.to_string_lossy().into_owned(),
        ]),
        // a container gets a terminal when there is one: docker refuses -t without it
        legionctl_api::Inside::Container => {
            if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                launch.sandbox.insert(2, "-t".into());
            }
        }
        legionctl_api::Inside::Vm => launch.sandbox.insert(0, exe),
    }
    let argv = [launch.scope, launch.sandbox, gate, launch.agent].concat();
    for line in &launch.lines {
        eprintln!("legionctl: {line}");
    }
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
                strings("cache")
                    .iter()
                    .map(|r: &String| r.parse())
                    .collect::<Result<_>>()?,
                match sub.get_one::<String>("url") {
                    Some(url) => proxy::Upstream::Url {
                        url: url.clone(),
                        headers: strings("header")
                            .iter()
                            .map(|h: &String| {
                                h.split_once('=')
                                    .map(|(n, v)| (n.to_string(), v.to_string()))
                            })
                            .collect::<Option<_>>()
                            .context("--header wants name=value")?,
                    },
                    None => proxy::Upstream::Command {
                        argv: strings("command"),
                        no_network: sub.get_flag("no-network"),
                    },
                },
            ));
        }
        Some(("tui", _)) => return tui::run(),
        Some(("vm", sub)) => {
            let spec = sub.get_one::<String>("spec").context("the spec")?;
            let agent = sub
                .get_many::<String>("agent")
                .into_iter()
                .flatten()
                .cloned()
                .collect();
            return vm::run(std::path::Path::new(spec), agent);
        }
        Some(("relay", sub)) => {
            let socket = sub.get_one::<String>("socket").context("the socket")?;
            return tokio::runtime::Runtime::new()?
                .block_on(vm::relay(std::path::Path::new(socket)));
        }
        Some(("forward", sub)) => {
            let port = *sub.get_one::<u16>("port").context("the port")?;
            let socket = sub.get_one::<String>("socket").context("the socket")?;
            let agent: Vec<String> = sub
                .get_many::<String>("agent")
                .into_iter()
                .flatten()
                .cloned()
                .collect();
            return vm::forward(port, std::path::Path::new(socket), &agent);
        }
        Some(("guest", _)) => return vm::guest(),
        Some(("windows-vm", _)) => return winvm::run(),
        Some(("trace", _)) => return trace(),
        Some(("run", sub)) => return run(sub),
        Some(("hook", sub)) => {
            return hook::run(sub.get_one::<String>("event").map_or("", String::as_str));
        }
        _ => {}
    }
    operation(&matches)
}

/// An operation of the registry: called, then shown as JSON or text.
fn operation(matches: &ArgMatches) -> Result<()> {
    let (op, input) = cli::operation(matches)?.context("not an operation")?;
    let json = matches.get_flag("json");
    let client = Client::new("cli")?;
    let (output, shown) = tokio::runtime::Runtime::new()?.block_on(async {
        if op.name == "session.run" && !json {
            run_streamed(&client, input).await
        } else {
            Ok((client.call_value(op.name, &input).await?, String::new()))
        }
    })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }
    if op.name == "session.start" || op.name == "session.resume" {
        return start_session(serde_json::from_value(output)?);
    }
    let (text, success) = legionctl_api::render(op.name, output)?;
    // a streamed answer was shown; the rest of it (the stream lags behind the run's end) and the result follow
    let rest = text.strip_prefix(shown.as_str()).unwrap_or(&text);
    if !rest.is_empty() {
        println!("{rest}");
    }
    if !success {
        std::process::exit(1);
    }
    Ok(())
}
