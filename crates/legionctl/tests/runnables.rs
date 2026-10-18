//! End-to-end: runnables and capability sets (DESIGN-13 M1) through the CLI and the real operator. A session start
//! requests sets (its harness's and its languages' by itself, the main agent's, its flags); the grant is the subset
//! that its environment and the user layer allow, a repository set's powerful parts only after the user's first-use
//! answer; anything else refuses the start with the reason. The grant is a fact (`runnables show`), its connections
//! are in the session's record for the gateway, and its storage and harness parts reach the sandbox.

mod common;
#[path = "common/desktop.rs"]
mod desktop;
#[path = "common/git.rs"]
mod git;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::Operator;
use common::scratch::Scratch;
use git::git;
use serde_json::{Value, json};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn text(out: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A test home with git's identity, workspaces under `worktrees`, a repository with `files` committed, and the real
/// operator on it.
struct Lab {
    op: Operator,
    home: PathBuf,
    repo: PathBuf,
}

impl Lab {
    /// With `gateway`, the model gateway on that port (its tunnel is the sessions' only way out).
    fn new(
        tag: &str,
        files: &[(&str, &str)],
        extra: &[(&str, OsString)],
        gateway: Option<u16>,
    ) -> Lab {
        // outside /tmp (private in a session sandbox) and outside any repository, like a real home
        let home = Scratch::new(format!(
            "/var/tmp/legionctl-runnables-{tag}-{}",
            std::process::id()
        ));
        write(
            &home.join(".gitconfig"),
            "[user]\n  name = Test\n  email = test@example.com\n[init]\n  defaultBranch = main\n",
        );
        write(
            &home.join(".config/legionctl/workspace.jsonc"),
            &format!(
                r#"{{ "workspace": {{ "root": "{}" }} }}"#,
                home.join("worktrees").display()
            ),
        );
        if let Some(port) = gateway {
            write(
                &home.join(".config/legionctl/gateway.jsonc"),
                &json!({"gateway": {"port": port}}).to_string(),
            );
        }
        std::fs::create_dir_all(home.join("root")).unwrap();
        let repo = home.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&home, &repo, &["init", "--quiet"]);
        write(&repo.join(".gitignore"), "/agentWorkspaces\n");
        for (name, content) in files {
            write(&repo.join(name), content);
        }
        git(&home, &repo, &["add", "."]);
        git(&home, &repo, &["commit", "--quiet", "-m", "start"]);
        let mut env = vec![
            ("LEGIONCTL_SYSROOT", home.join("root").into()),
            ("HOME", OsString::from(&*home)),
            ("PATH", path_with_build()),
            (
                "LEGIONCTL_PLUGIN",
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../integrations/agent-plugin")
                    .into(),
            ),
        ];
        env.extend(extra.iter().cloned());
        let mut op = Operator::start(tag, &env);
        let path = home.to_path_buf();
        op.held.push(home);
        Lab {
            op,
            home: path,
            repo,
        }
    }

    /// The session agents of the user layer (`session.agents`), with no scope (the tests have no user systemd).
    fn agents(&self, agents: &Value) {
        write(
            &self.home.join(".config/legionctl/session.jsonc"),
            &json!({"session": {"agents": agents, "scope": false}}).to_string(),
        );
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.op.run(
            &self.repo,
            &[("HOME", self.home.to_str().unwrap())],
            args,
            "",
        )
    }

    /// `session start <agent> --dossier <dossier> --json <more>`.
    fn start(&self, agent: &str, dossier: &str, more: &[&str]) -> Output {
        let mut args = vec!["session", "start", agent, "--dossier", dossier, "--json"];
        args.extend(more);
        self.cli(&args)
    }

    /// `runnables show <target> --json`.
    fn show(&self, target: &str) -> Value {
        serde_json::from_str(&self.op.ok(&["runnables", "show", target, "--json"])).unwrap()
    }

    /// The sandbox, the git gate and the agent of `launch`, run as the client does.
    fn run_launch(&self, launch: &Value) -> Output {
        let strings = |v: &Value| -> Vec<String> {
            v.as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap().to_string())
                .collect()
        };
        let gate = [
            "--ro-bind",
            env!("CARGO_BIN_EXE_legionctl"),
            "/usr/bin/git",
            "--setenv",
            "LEGIONCTL_REAL_GIT",
            launch["real_git"].as_str().unwrap(),
        ]
        .map(String::from);
        let argv = [
            strings(&launch["sandbox"]),
            gate.to_vec(),
            strings(&launch["agent"]),
        ]
        .concat();
        Command::new(&argv[0])
            .args(&argv[1..])
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", path_with_build())
            .env("XDG_RUNTIME_DIR", &*self.op.runtime)
            .output()
            .unwrap()
    }

    /// The `connections` of session `id`'s record, where the gateway reads them.
    fn record_connections(&self, id: &str) -> Value {
        let db = rusqlite::Connection::open(self.home.join(".local/state/legionctl/operator.db"))
            .unwrap();
        let text: String = db
            .query_row(
                "SELECT connections FROM sessions WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        serde_json::from_str(&text).unwrap()
    }
}

/// The launch of a start that must succeed.
fn launch(out: &Output) -> Value {
    assert!(out.status.success(), "{}", text(out));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn path_with_build() -> OsString {
    let bin = Path::new(env!("CARGO_BIN_EXE_legionctl")).parent().unwrap();
    std::env::join_paths(
        std::iter::once(bin.to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap()
}

fn hosts(connections: &Value) -> Vec<String> {
    connections["out"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["host"].as_str().unwrap().to_string())
        .collect()
}

fn set_names(grant: &Value) -> Vec<String> {
    grant["sets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect()
}

/// "What they need they tell, and only that they get" (DESIGN-13): a start that asks for a set no layer has, a user
/// set that the user layer does not allow, a set that runs only in a VM, and a set whose storage reaches a hidden
/// folder (~/.ssh) is refused at its start, with one reason per set, before anything is bound: no session, the
/// dossier stays free. The same start without those flags gets the main agent's sets.
#[test]
fn a_start_that_requests_sets_it_may_not_have_is_refused_with_each_reason() {
    let lab = Lab::new("refuse", &[], &[], None);
    lab.agents(&json!({"probe": ["true"]}));
    let sets = lab.home.join(".config/legionctl/capabilities");
    write(
        &sets.join("deploy.jsonc"),
        r#"{ "name": "deploy", "version": 1, "connections": { "out": [{ "host": "deploy.example.com" }] } }"#,
    );
    write(
        &sets.join("vm-only.jsonc"),
        r#"{ "name": "vm-only", "version": 1, "sandbox": ["vm"] }"#,
    );
    write(
        &sets.join("keys.jsonc"),
        r#"{ "name": "keys", "version": 1, "storage": [{ "path": "~/.ssh/known", "mode": "read" }] }"#,
    );
    write(
        &lab.home.join(".config/legionctl/runnables.jsonc"),
        r#"{ "runnables": { "allow+": ["vm-only", "keys"] } }"#,
    );
    let refused = lab.start(
        "probe",
        "refuse",
        &[
            "--capability",
            "no-such-set",
            "--capability",
            "deploy",
            "--capability",
            "vm-only",
            "--capability",
            "keys",
        ],
    );
    let why = text(&refused);
    assert!(!refused.status.success(), "{why}");
    for reason in [
        "capability sets refused at start",
        "no-such-set: no such capability set",
        "deploy: your config layer does not allow it (runnables.allow)",
        "vm-only: it runs only in vm; this session runs in host",
        "keys: storage ~/.ssh/known: it reaches",
    ] {
        assert!(why.contains(reason), "{reason}: {why}");
    }
    let sessions = lab.op.cli_in(&lab.repo, &["session", "list", "--json"]);
    assert_eq!(
        serde_json::from_slice::<Value>(&sessions.stdout).unwrap(),
        json!([]),
        "a refused start holds nothing"
    );
    // a headless run takes the same grant (refused before its ACP adapter starts)
    let run = lab.cli(&[
        "session",
        "run",
        "claude",
        "hi",
        "--dossier",
        "refuse",
        "--capability",
        "no-such-set",
    ]);
    assert!(!run.status.success(), "{}", text(&run));
    assert!(
        text(&run).contains("no-such-set: no such capability set"),
        "{}",
        text(&run)
    );
    // runnables check gives the same reason before any start
    let check: Value =
        serde_json::from_slice(&lab.op.cli(&["runnables", "check", "--json"]).stdout).unwrap();
    let deploy = check["sets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "deploy")
        .unwrap_or_else(|| panic!("{check:#}"));
    assert_eq!(
        deploy["refusal"],
        "your config layer does not allow it (runnables.allow)"
    );

    let launch = launch(&lab.start("probe", "refuse", &[]));
    let grant = lab.show(launch["id"].as_str().unwrap());
    // the config's sets, then the toolsets of the main-agent's skills (DESIGN/22 rule 2)
    assert_eq!(
        set_names(&grant),
        [
            "web-read",
            "github-read",
            "dossier-write",
            "machine-read",
            "workspace-write",
            "code-work",
            "agent-work",
            "thread-member",
            "views-write",
            "session-own",
            "mcp-tools"
        ],
        "{grant:#}"
    );
}

/// A session in a Rust repository gets rust-dev by itself (config/languages/rust.jsonc names it): cargo's hosts are
/// in its granted manifest and in its session record, where the gateway reads them, and cargo's registry is an
/// overlay: the agent writes it (the home is read-only in the sandbox) and the real folder stays as it was.
#[test]
fn a_rust_session_gets_cargos_hosts_in_its_record_and_its_registry_as_an_overlay() {
    let lab = Lab::new(
        "rust",
        &[(
            "Cargo.toml",
            "[package]\nname = \"probe\"\nversion = \"0.1.0\"\n",
        )],
        &[],
        None,
    );
    lab.agents(&json!({"probe": ["sh", "-c",
        "echo crate > ~/.cargo/registry/probe.crate && cat ~/.cargo/registry/probe.crate && echo WROTE"]}));
    let launch = launch(&lab.start("probe", "rust", &[]));
    let id = launch["id"].as_str().unwrap();
    let lines = launch["lines"].to_string();
    assert!(lines.contains("rust-dev@1"), "{lines}");

    let grant = lab.show(id);
    assert!(
        set_names(&grant).contains(&"rust-dev".to_string()),
        "{grant:#}"
    );
    let crates = ["index.crates.io", "crates.io", "static.crates.io"];
    let granted = hosts(&grant["manifest"]["connections"]);
    let recorded = hosts(&lab.record_connections(id));
    for host in crates {
        assert!(granted.contains(&host.to_string()), "{granted:?}");
        assert!(recorded.contains(&host.to_string()), "{recorded:?}");
    }
    assert_eq!(
        grant["manifest"]["storage"][0],
        json!({"path": "~/.cargo/registry", "mode": "overlay"})
    );

    let out = lab.run_launch(&launch);
    assert!(text(&out).contains("crate\nWROTE"), "{}", text(&out));
    assert!(
        !lab.home.join(".cargo/registry/probe.crate").exists(),
        "the write stays in the session's layer"
    );
}

/// The harness sets (DESIGN-12, user decision 2026-10-17: the harness's own sandbox is off, ours is the one): a
/// session of agent claude gets claude-code by itself, its settings file has Claude Code's sandbox off beside the
/// git gate's denies, and the telemetry variables are set; a session of agent codex gets Codex's sandbox mode
/// danger-full-access and its update check off after its command.
#[test]
fn the_harness_sets_turn_the_harness_sandboxes_off_in_the_session() {
    let lab = Lab::new("harness", &[], &[], None);
    lab.agents(&json!({"claude": ["claude", "--settings", "{settings}"], "codex": ["codex"]}));
    let claude = launch(&lab.start("claude", "harness", &[]));
    let id = claude["id"].as_str().unwrap();
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(
            lab.op
                .runtime
                .join("legionctl/sessions")
                .join(id)
                .join("claude-settings.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(settings["sandbox"]["enabled"], false, "{settings:#}");
    assert!(
        settings["permissions"]["deny"]
            .as_array()
            .unwrap()
            .contains(&json!("Bash(git push:*)")),
        "{settings:#}"
    );
    let sandbox = claude["sandbox"].to_string();
    assert!(
        sandbox.contains(r#""--setenv","DISABLE_TELEMETRY","1""#)
            && sandbox.contains(r#""--setenv","DISABLE_AUTOUPDATER","1""#),
        "{sandbox}"
    );
    assert_eq!(set_names(&lab.show(id))[0], "claude-code");

    let codex = launch(&lab.start("codex", "harness-codex", &[]));
    let agent: Vec<&str> = codex["agent"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    let at = agent.iter().position(|a| *a == "codex").unwrap();
    assert_eq!(
        agent[at..],
        [
            "codex",
            "-c",
            "check_for_update_on_startup=false",
            "-c",
            "sandbox_mode=\"danger-full-access\""
        ]
    );
}

/// Trust on first use for a repository's set (O-2): a set in `.legionctl/capabilities` with a powerful part
/// (connections) refuses the start until the user allowed its content once, through the desktop question; then the
/// start gets it. A changed file refuses again and asks again; after Deny it stays refused and nobody is asked again
/// for that content.
#[tokio::test(flavor = "multi_thread")]
async fn a_repository_set_with_a_powerful_part_is_granted_only_after_the_user_allows_its_content() {
    let runtime = Scratch::new(format!("/var/tmp/lc-caps-bus-{}", std::process::id()));
    let bus_path = runtime.join("bus");
    let _bus = common::process::Kill::spawn(
        std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--nopidfile"])
            .arg(format!("--address=unix:path={}", bus_path.display())),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !bus_path.exists() {
        assert!(std::time::Instant::now() < deadline, "the private bus");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let address = format!("unix:path={}", bus_path.display());
    let (_desktop, asked) = desktop::answering(&address, &["allow", "deny"]).await;
    let lab = Lab::new(
        "tofu",
        &[],
        &[("DBUS_SESSION_BUS_ADDRESS", address.into())],
        None,
    );
    lab.agents(&json!({"probe": ["true"]}));
    let set = lab.repo.join(".legionctl/capabilities/mirror.jsonc");
    let mirror = |host: &str| {
        write(
            &set,
            &format!(
                r#"{{ "name": "mirror", "version": 1, "connections": {{ "out": [{{ "host": "{host}" }}] }} }}"#
            ),
        );
    };
    let approvals = lab.home.join(".local/state/legionctl/approvals.json");
    let answered = |content: &str, n: usize| {
        for _ in 0..200 {
            if std::fs::read_to_string(&approvals)
                .unwrap_or_default()
                .contains(content)
                && asked.lock().unwrap().len() >= n
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("no answer stored: {:?}", asked.lock().unwrap());
    };
    let start = |dossier: &str| lab.start("probe", dossier, &["--capability", "mirror"]);

    mirror("mirror.example.com");
    let waits = start("tofu-1");
    assert!(
        !waits.status.success()
            && text(&waits).contains("sets connections and waits for your first answer"),
        "{}",
        text(&waits)
    );
    answered("true", 1);
    let launch = launch(&start("tofu-1"));
    let grant = lab.show(launch["id"].as_str().unwrap());
    let granted = grant["sets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "mirror")
        .unwrap_or_else(|| panic!("{grant:#}"));
    assert_eq!(granted["layer"], "repo");
    assert!(hosts(&grant["manifest"]["connections"]).contains(&"mirror.example.com".to_string()));
    let question = asked.lock().unwrap()[0].clone();
    assert!(
        question.contains("mirror.jsonc") && question.contains("capabilities.mirror.connections"),
        "{question}"
    );

    mirror("other.example.com");
    let changed = start("tofu-2");
    assert!(
        !changed.status.success() && text(&changed).contains("waits for your first answer"),
        "{}",
        text(&changed)
    );
    answered("false", 2);
    let denied = start("tofu-2");
    assert!(
        !denied.status.success() && text(&denied).contains("you denied the content of"),
        "{}",
        text(&denied)
    );
    assert_eq!(
        asked.lock().unwrap().len(),
        2,
        "a changed file asks again, a denied content does not"
    );
}

/// A server on the host's 127.0.0.1 (its port) that answers each HTTP request with `TUNNEL_OK`.
fn tunnel_ok_server() -> u16 {
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = server.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut stream in server.incoming().flatten() {
            let mut request = [0u8; 1024];
            let _ = std::io::Read::read(&mut stream, &mut request);
            let _ = std::io::Write::write_all(
                &mut stream,
                b"HTTP/1.1 200 OK\r\ncontent-length: 9\r\nconnection: close\r\n\r\nTUNNEL_OK",
            );
        }
    });
    port
}

/// `launch` run as `session start` runs it: its sandbox in the session's scope cgroup (`legionctl-session-<id>.scope`,
/// here under this test's own cgroup), where the gateway finds the session of a caller.
fn run_in_scope(lab: &Lab, launch: &Value) -> Output {
    let own = std::fs::read_to_string("/proc/self/cgroup").unwrap();
    let scope = Path::new("/sys/fs/cgroup")
        .join(own.trim().trim_start_matches("0::/"))
        .join(format!(
            "legionctl-session-{}.scope",
            launch["id"].as_str().unwrap()
        ));
    std::fs::create_dir_all(&scope).unwrap();
    let mut wrapped = launch.clone();
    let procs = scope.join("cgroup.procs");
    let sandbox = wrapped["sandbox"].as_array_mut().unwrap();
    for (at, part) in [
        "sh",
        "-c",
        "echo $$ > \"$0\" && exec \"$@\"",
        procs.to_str().unwrap(),
    ]
    .into_iter()
    .enumerate()
    {
        sandbox.insert(at, part.into());
    }
    let out = lab.run_launch(&wrapped);
    let _ = std::fs::remove_dir(&scope);
    out
}

/// A session's only way out is the gateway's tunnel to its granted connections (DESIGN-12; todo-205: Codex on a
/// `ChatGPT` login reaches chatgpt.com only this way, through its `HTTPS_PROXY`). A program in the sandbox (curl, as
/// Codex's HTTP client does) opens a tunnel to a declared endpoint and gets the server's answer; a host it was not
/// granted (the same server by its address) and a forbidden one are refused with the reason. Each tunnel is an event
/// with its bytes, each refusal one with its reason. The sandbox runs in the session's scope cgroup, as `session start`
/// puts it there (the gateway knows the session by it).
#[test]
fn a_session_reaches_its_granted_connections_through_the_gateway_tunnel_and_nothing_else() {
    let port = tunnel_ok_server();
    let gateway = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let lab = Lab::new("tunnel", &[], &[], Some(gateway));
    write(
        &lab.home
            .join(".config/legionctl/capabilities/service.jsonc"),
        &json!({"name": "service", "version": 1, "connections": {
            "out": [{"host": "localhost", "port": port, "why": "the test's server"}],
            "forbidden": ["forbidden.localhost"]}})
        .to_string(),
    );
    write(
        &lab.home.join(".config/legionctl/runnables.jsonc"),
        r#"{ "runnables": { "allow+": ["service"] } }"#,
    );
    // curl through the session's proxy: the answer of a granted host, then the tunnel status of two others
    let through = |host: &str| {
        format!(
            "curl -s -w ' %{{http_connect}}\\n' --noproxy '' -p -x \"$HTTPS_PROXY\" http://{host}:{port}/"
        )
    };
    let probe = [
        through("localhost"),
        through("127.0.0.1"),
        through("forbidden.localhost"),
    ]
    .join("; ");
    lab.agents(&json!({"probe": ["sh", "-c", probe]}));
    let launch = launch(&lab.start("probe", "tunnel", &["--capability", "service"]));
    let id = launch["id"].as_str().unwrap();
    let lines = launch["lines"].to_string();
    assert!(
        lines.contains("network: the model gateway and the granted connections only"),
        "{lines}"
    );
    let out = run_in_scope(&lab, &launch);
    let said = text(&out);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        ["TUNNEL_OK 200", " 403", " 403"],
        "granted, not granted, forbidden: {said}"
    );
    let events: Value =
        serde_json::from_str(&lab.op.ok(&["events", "--limit", "1000", "--json"])).unwrap();
    let of = |kind: &str| -> Vec<Value> {
        events["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == kind)
            .map(|e| e["payload"].clone())
            .collect()
    };
    let tunnels = of("gateway.tunnel");
    assert_eq!(tunnels.len(), 1, "{events:#}");
    assert_eq!(tunnels[0]["host"], "localhost");
    assert_eq!(tunnels[0]["who"], format!("session:{id}"));
    assert!(tunnels[0]["down"].as_u64().unwrap() > 9, "{tunnels:?}");
    let refused: Vec<(String, String)> = of("gateway.refused")
        .iter()
        .map(|e| {
            (
                e["host"].as_str().unwrap().to_string(),
                e["why"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        refused,
        [
            (
                "127.0.0.1".to_string(),
                "this session has no connection to it (`legionctl runnables show <session>`)"
                    .to_string()
            ),
            (
                "forbidden.localhost".to_string(),
                "this session may never reach it".to_string()
            )
        ]
    );
}
