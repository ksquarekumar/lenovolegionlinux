//! `legionctl`: the CLI and MCP server of legionctl. Every command is an operation of `legionctl-operator` (see
//! `legionctl-api`); only `mcp` and `run` are local, because `mcp` is a surface and `run` must run in this process.

mod hook;
mod mcp;
mod proxy;
mod vm;
mod winvm;

use std::os::unix::process::CommandExt;

use anyhow::{Context, Result, ensure};
use clap::{Arg, ArgAction, ArgMatches, Command};
use legionctl_api::{ClientPath, Cores, GateIn, Launch, RunEnvIn, RunGpu, cli, op};

use legionctl_api::client::Client;

fn client_only() -> Vec<Command> {
    vec![
        Command::new("mcp")
            .about("MCP server over stdio, for Claude Code, Codex and other agents")
            .arg(
                Arg::new("only")
                    .long("only")
                    .action(ArgAction::Append)
                    .help("An operation the server offers (a bound agent's tools); none: every public operation"),
            ),
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
            .arg(
                Arg::new("command")
                    .num_args(1..)
                    .allow_hyphen_values(true)
                    .required_unless_present("url")
                    .last(true),
            ),
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
        Command::new("windows-vm-stop").hide(true),
        Command::new("trace").about("Follow the event log: agents, people and the machine daemon, as it happens"),
        Command::new("hook")
            .about("Claude Code hook adapter: reads the hook's JSON from stdin, answers in Claude Code's hook protocol")
            // any words: the adapter refuses an unknown event itself (exit 1), as clap's exit 2 blocks the agent
            .arg(
                Arg::new("event")
                    .num_args(0..)
                    .trailing_var_arg(true)
                    .allow_hyphen_values(true)
                    .value_name("EVENT")
                    .help(format!("One of: {}", legionctl_core::plugin::HOOK_EVENTS.join(", "))),
            ),
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
///
/// # Errors
/// `/proc/self/stat` cannot be read or has no niceness.
fn current_nice() -> Result<i32> {
    let stat = std::fs::read_to_string("/proc/self/stat").context("reading /proc/self/stat")?;
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().nth(16)?.parse().ok())
        .with_context(|| format!("/proc/self/stat has no niceness: {stat}"))
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
            vec![
                "nice".into(),
                "-n".into(),
                (n - current_nice()?).to_string(),
            ],
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
    let real = std::env::var_os("LEGIONCTL_REAL_GIT")
        .context("LEGIONCTL_REAL_GIT is not set (outside a session?)")?;
    if stash_step(&real) {
        let err = std::process::Command::new(real).args(args).exec();
        return Err(err).context("running the real git");
    }
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
    let err = std::process::Command::new(real).args(args).exec();
    Err(err).context("running the real git")
}

/// Whether this git call is a step of a `git stash` that the gate let run: the real git, running `stash`, is the
/// parent (a stash runs `git reset --hard` itself, and `git clean` for its untracked files).
fn stash_step(real: &std::ffi::OsStr) -> bool {
    let parent = std::os::unix::process::parent_id();
    let (Ok(exe), Ok(real), Ok(line)) = (
        std::fs::read_link(format!("/proc/{parent}/exe")),
        std::fs::canonicalize(real),
        std::fs::read(format!("/proc/{parent}/cmdline")),
    ) else {
        return false;
    };
    let args: Vec<String> = line
        .split(|b| *b == 0)
        .skip(1)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    exe == real && legionctl_core::gates::subcommand(&args) == Some("stash")
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
    // a session may not follow the stream: its run is shown when it ends
    let events = match client.follow("now").await {
        Ok(events) => events,
        Err(e) if legionctl_core::error::code_of(&e) == legionctl_api::Code::Denied => {
            return Ok((client.call_value("session.run", &input).await?, shown));
        }
        Err(e) => return Err(e),
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
                // a note (the adapter's install) is not part of the answer
                if payload["note"] != true {
                    shown.push_str(text);
                }
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

/// The remote MCP server's headers from the proxy's environment (never its command line, where the exec sensor sees
/// them).
fn proxy_headers() -> Result<Vec<(String, String)>> {
    let Ok(json) = std::env::var(legionctl_core::session::HEADERS_ENV) else {
        return Ok(Vec::new());
    };
    let headers: std::collections::BTreeMap<String, String> = serde_json::from_str(&json)
        .with_context(|| {
            format!(
                "{} is not a JSON object of strings",
                legionctl_core::session::HEADERS_ENV
            )
        })?;
    Ok(headers.into_iter().collect())
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
        Some(("mcp", sub)) => {
            let only: Vec<String> = sub
                .get_many::<String>("only")
                .into_iter()
                .flatten()
                .cloned()
                .collect();
            return tokio::runtime::Runtime::new()?.block_on(mcp::serve(&only));
        }
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
                        headers: proxy_headers()?,
                    },
                    None => proxy::Upstream::Command {
                        argv: strings("command"),
                        no_network: sub.get_flag("no-network"),
                    },
                },
            ));
        }
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
        Some(("windows-vm-stop", _)) => return winvm::stop(),
        Some(("trace", _)) => return trace(),
        Some(("run", sub)) => return run(sub),
        Some(("hook", sub)) => {
            let args: Vec<String> = sub
                .get_many::<String>("event")
                .into_iter()
                .flatten()
                .cloned()
                .collect();
            hook::run(&args);
            return Ok(());
        }
        _ => {}
    }
    operation(&matches)
}

/// `sensors --watch`: the view again each second until Ctrl+C (each view one JSON line with `--json`).
fn watch(client: &Client, name: &str, input: &serde_json::Value, json: bool) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    loop {
        let output = runtime.block_on(client.call_value(name, input))?;
        if json {
            println!("{}", serde_json::to_string(&output)?);
        } else {
            // clear the screen, then the view from the top
            println!("\x1b[2J\x1b[H{}", legionctl_api::render(name, output)?.0);
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// An operation of the registry: called, then shown as JSON or text.
fn operation(matches: &ArgMatches) -> Result<()> {
    let (op, input) = cli::operation(matches, true)?.context("not an operation")?;
    let json = matches.get_flag("json");
    let client = Client::new("cli")?;
    if op.name == "sensors" && input["watch"] == true {
        return watch(&client, op.name, &input, json);
    }
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
    // a resource's start of a session runs the session in this terminal too
    if op.name == "resources.act"
        && let Ok(acted) = serde_json::from_value::<legionctl_api::Acted>(output.clone())
        && (acted.op == "session.start" || acted.op == "session.resume")
        && let Some(launch) = acted.output
    {
        return start_session(serde_json::from_value(launch)?);
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
