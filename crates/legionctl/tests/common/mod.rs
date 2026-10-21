//! The real operator on a test socket, started the way systemd starts it (socket activation), and the CLI against
//! it. The operator gets only the environment a test gives it; the D-Bus addresses point nowhere, so its session
//! module stops and machine changes cannot reach a daemon.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

pub mod process;
pub mod scratch;

use process::Kill;
use scratch::Scratch;

/// Another package's binary `name` beside this test's `legionctl`, refused when it is older than a source it was built
/// from: `cargo test -p legionctl` builds only `legionctl`, so an old daemon or operator would give false results.
pub fn sibling(name: &str) -> PathBuf {
    fresh(&Path::new(env!("CARGO_BIN_EXE_legionctl")).with_file_name(name))
}

/// `bin`, when no source in its dep-info (`<bin>.d`, which cargo writes with each build) is newer than it.
pub fn fresh(bin: &Path) -> PathBuf {
    fresh_as(bin, None, "cargo build --profile test-fast")
}

/// [`fresh`] with each dep-info path under `seen.0` read under `seen.1`, and `rebuild` as the command that helps.
pub fn fresh_as(bin: &Path, seen: Option<(&str, &Path)>, rebuild: &str) -> PathBuf {
    let missing = format!("{} is missing: run {rebuild} first", bin.display());
    let built = std::fs::metadata(bin)
        .and_then(|m| m.modified())
        .expect(&missing);
    let deps = std::fs::read_to_string(bin.with_extension("d")).expect(&missing);
    let (_, sources) = deps
        .lines()
        .next()
        .unwrap_or_default()
        .split_once(": ")
        .unwrap_or_default();
    for source in sources.split_whitespace() {
        let source = match seen.and_then(|(at, root)| {
            Some(root.join(source.strip_prefix(at)?.trim_start_matches('/')))
        }) {
            Some(here) => here,
            None => PathBuf::from(source),
        };
        // a removed source changed the file that named it, which is still in the list
        if let Ok(changed) = std::fs::metadata(&source).and_then(|m| m.modified()) {
            assert!(
                changed <= built,
                "{} is older than this build: run {rebuild} first ({} changed)",
                bin.display(),
                source.display()
            );
        }
    }
    bin.to_path_buf()
}

/// The plugin's skills that declare toolsets: an agent that loaded them all has the scope of its work.
pub const WORK_SKILLS: &[&str] = &[
    "dossier-practice",
    "workspace-practice",
    "verify-and-ship",
    "sessions-and-agents",
    "glanceable-style",
    "tool-lifecycle",
    "machine-tools",
];

/// The (HOME, agent session) pairs whose agent loaded [`WORK_SKILLS`].
static LOADED: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

pub struct Operator {
    /// Short on purpose: a unix socket path must fit in 108 bytes.
    pub runtime: Scratch,
    /// The operator (systemd-socket-activate execs it).
    pub child: Kill,
    /// The test's other folders (its HOME): they go after the operator stopped.
    pub held: Vec<Scratch>,
}

/// The operator's HOME: the test's (it must be a scratch folder), else its runtime folder. The shipped
/// `workspace.root` is under it, so no test makes worktrees or thread folders in the user's real folders (CI run
/// 39189528444 made /data/worktrees/threads/thread-1 when that was the default).
fn scratch_home(env: &[(&str, OsString)], runtime: &Path) -> PathBuf {
    let home = env
        .iter()
        .find(|(k, _)| *k == "HOME")
        .map_or_else(|| runtime.to_path_buf(), |(_, h)| PathBuf::from(h));
    scratch_only(&home);
    home
}

/// `dir` (made when it is not there) is inside a test's scratch folder (under /var/tmp or the temp folder) once `..`
/// and symbolic links are resolved: the harness never writes into the user's own folders.
fn scratch_only(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let real = std::fs::canonicalize(dir).unwrap();
    let inside = [PathBuf::from("/var/tmp"), std::env::temp_dir()]
        .iter()
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .any(|root| real.starts_with(&root) && real != root);
    assert!(
        inside,
        "{} ({}) is not in a test's scratch folder",
        dir.display(),
        real.display()
    );
}

impl Operator {
    pub fn start(tag: &str, env: &[(&str, OsString)]) -> Operator {
        Self::start_under(tag, env, &[])
    }

    /// As [`Operator::start`], with `wrapper` (a tracer: `strace -o <log> ...`) in front of the socket activation.
    pub fn start_under(tag: &str, env: &[(&str, OsString)], wrapper: &[OsString]) -> Operator {
        // outside /tmp like /run/user/<uid>: the session sandbox replaces /tmp, which would hide what it must hide
        let runtime = Scratch::new(format!("/var/tmp/lc-{tag}-{}", std::process::id()));
        let socket = runtime.join("legionctl/api.sock");
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let nowhere = OsString::from("unix:path=/nonexistent/bus");
        let mut cmd = match wrapper {
            [first, rest @ ..] => {
                let mut cmd = Command::new(first);
                // the wrapper runs with the operator's environment, which has no PATH of the test's
                let activate = std::env::split_paths(&std::env::var_os("PATH").unwrap())
                    .map(|d| d.join("systemd-socket-activate"))
                    .find(|p| p.is_file())
                    .expect("systemd-socket-activate is installed");
                cmd.args(rest).arg(activate);
                cmd
            }
            [] => Command::new("systemd-socket-activate"),
        };
        cmd.env_clear().arg("-l").arg(&socket);
        // the buses and Docker point nowhere unless the test gives its own: no test reaches the user's own containers
        // (WinBoat) or buses by default; the plugin is the checkout's, never the installed package's
        let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../integrations/agent-plugin");
        let defaults = [
            ("LEGIONCTL_PLUGIN", plugin.into_os_string()),
            ("DBUS_SESSION_BUS_ADDRESS", nowhere.clone()),
            ("DBUS_SYSTEM_BUS_ADDRESS", nowhere),
            ("DOCKER_HOST", "unix:///nonexistent/docker.sock".into()),
            ("XDG_RUNTIME_DIR", runtime.as_os_str().to_owned()),
        ];
        let home = scratch_home(env, &runtime);
        let defaults = defaults
            .into_iter()
            .chain(std::iter::once(("HOME", home.into_os_string())));
        let unset = defaults.filter(|(k, _)| !env.iter().any(|(e, _)| e == k));
        for (name, value) in env.iter().cloned().chain(unset) {
            // the user's own clauth never runs in a test: a folder of PATH outside the tests' homes that has one is
            // left out (a test gives its stand-in in its home)
            let value = if name == "PATH" {
                std::env::join_paths(
                    std::env::split_paths(&value)
                        .filter(|d| d.starts_with("/var/tmp") || !d.join("clauth").exists()),
                )
                .unwrap()
            } else {
                value
            };
            cmd.arg(format!("--setenv={name}={}", value.to_string_lossy()));
        }
        let child = Kill::spawn(cmd.arg(sibling("legionctl-operator")));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "the test socket did not appear");
            std::thread::sleep(Duration::from_millis(20));
        }
        Operator {
            runtime,
            child,
            held: Vec::new(),
        }
    }

    /// `legionctl` with only `XDG_RUNTIME_DIR` (to find the socket), `PATH`, `env`, the working directory `cwd` and
    /// `stdin` as its input. An agent session (`LEGIONCTL_SESSION`, `CODEX_THREAD_ID` or `CLAUDE_CODE_SESSION_ID` in
    /// `env`, the first one that is set, as the client takes it) loads the skills of its work at its first call, as an
    /// agent does when its reminders name them, so their toolsets are its scope (DESIGN/22 rule 2);
    /// [`Operator::run_bare`] is an agent that loaded none.
    pub fn run(&self, cwd: &Path, env: &[(&str, &str)], args: &[&str], stdin: &str) -> Output {
        let session_vars = [
            "LEGIONCTL_SESSION",
            "CODEX_THREAD_ID",
            "CLAUDE_CODE_SESSION_ID",
        ];
        if let Some(session) = session_vars
            .iter()
            .find_map(|v| env.iter().find(|(k, _)| k == v).map(|(_, s)| s))
        {
            // the grant is in the HOME's operator database: an operator that starts again on it keeps it
            let home = env.iter().find(|(k, _)| *k == "HOME").map_or_else(
                || self.runtime.display().to_string(),
                |(_, h)| (*h).to_string(),
            );
            let key = (home, (*session).to_string());
            if !LOADED.lock().unwrap().contains(&key) {
                let rest: Vec<(&str, &str)> = env
                    .iter()
                    .copied()
                    .filter(|(k, _)| !session_vars.contains(k))
                    .collect();
                // a test of a broken config or a busy database sees that in its own call: the next call loads again
                if self.skills_loaded(cwd, &rest, session, WORK_SKILLS) {
                    LOADED.lock().unwrap().push(key);
                }
            }
        }
        self.run_bare(cwd, env, args, stdin)
    }

    /// [`Operator::run`] with no skills loaded for its agent session.
    pub fn run_bare(&self, cwd: &Path, env: &[(&str, &str)], args: &[&str], stdin: &str) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_legionctl"))
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("XDG_RUNTIME_DIR", &*self.runtime)
            .env("PATH", std::env::var_os("PATH").unwrap())
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    /// The agent of `session` loads the legionctl `skills`, as Claude Code's `Skill` call reaches the post-tool hook
    /// (tests/fixtures/claude-code-hooks/post-tool-skill.json): their toolsets join its scope (DESIGN/22 rule 2). False
    /// when a hook call failed.
    fn skills_loaded(
        &self,
        cwd: &Path,
        env: &[(&str, &str)],
        session: &str,
        skills: &[&str],
    ) -> bool {
        for skill in skills {
            let payload = serde_json::json!({
                "session_id": session, "cwd": cwd, "hook_event_name": "PostToolUse", "tool_name": "Skill",
                "tool_input": {"skill": format!("legionctl:{skill}")},
                "tool_response": {"success": true, "commandName": format!("legionctl:{skill}")}
            });
            let out = self.run_bare(
                cwd,
                &[env, &[("LEGIONCTL_SESSION", session)]].concat(),
                &["hook", "post-tool"],
                &payload.to_string(),
            );
            if !out.status.success() {
                eprintln!("loading {skill}: {}", String::from_utf8_lossy(&out.stderr));
                return false;
            }
        }
        true
    }

    pub fn cli_in(&self, cwd: &Path, args: &[&str]) -> Output {
        self.run(cwd, &[], args, "")
    }

    pub fn cli(&self, args: &[&str]) -> Output {
        self.cli_in(&self.runtime, args)
    }

    /// stdout of a command that must succeed.
    pub fn ok(&self, args: &[&str]) -> String {
        let out = self.cli(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
}

impl Drop for Operator {
    /// The operator stops before its folders go.
    fn drop(&mut self) {
        self.child.stop();
        self.held.clear();
    }
}
