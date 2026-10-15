//! The real operator on a test socket, started the way systemd starts it (socket activation), and the CLI against
//! it. The operator gets only the environment a test gives it; the D-Bus addresses point nowhere, so its session
//! module stops and machine changes cannot reach a daemon.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

pub mod scratch;

use scratch::Scratch;

/// Another package's binary `name` beside this test's `legionctl`, refused when it is older than a source it was built
/// from: `cargo test -p legionctl` builds only `legionctl`, so an old daemon or operator would give false results.
pub fn sibling(name: &str) -> PathBuf {
    fresh(&Path::new(env!("CARGO_BIN_EXE_legionctl")).with_file_name(name))
}

/// `bin`, when no source in its dep-info (`<bin>.d`, which cargo writes with each build) is newer than it.
pub fn fresh(bin: &Path) -> PathBuf {
    let missing = format!(
        "{} is missing: run cargo build --profile test-fast first",
        bin.display()
    );
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
        // a removed source changed the file that named it, which is still in the list
        if let Ok(changed) = std::fs::metadata(source).and_then(|m| m.modified()) {
            assert!(
                changed <= built,
                "{} is older than this build: run cargo build --profile test-fast first ({source} changed)",
                bin.display()
            );
        }
    }
    bin.to_path_buf()
}

pub struct Operator {
    /// Short on purpose: a unix socket path must fit in 108 bytes.
    pub runtime: Scratch,
    /// The operator (systemd-socket-activate execs it).
    pub child: Child,
    /// The test's other folders (its HOME): they go after the operator stopped.
    pub held: Vec<Scratch>,
}

impl Operator {
    pub fn start(tag: &str, env: &[(&str, OsString)]) -> Operator {
        // outside /tmp like /run/user/<uid>: the session sandbox replaces /tmp, which would hide what it must hide
        let runtime = Scratch::new(format!("/var/tmp/lc-{tag}-{}", std::process::id()));
        let socket = runtime.join("legionctl/api.sock");
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let nowhere = OsString::from("unix:path=/nonexistent/bus");
        let mut cmd = Command::new("systemd-socket-activate");
        cmd.env_clear().arg("-l").arg(&socket);
        // the buses and Docker point nowhere unless the test gives its own: no test reaches the user's own containers
        // (WinBoat) or buses by default
        let defaults = [
            ("DBUS_SESSION_BUS_ADDRESS", nowhere.clone()),
            ("DBUS_SYSTEM_BUS_ADDRESS", nowhere),
            ("DOCKER_HOST", "unix:///nonexistent/docker.sock".into()),
            ("XDG_RUNTIME_DIR", runtime.as_os_str().to_owned()),
        ];
        let unset = defaults
            .into_iter()
            .filter(|(k, _)| !env.iter().any(|(e, _)| e == k));
        for (name, value) in env.iter().cloned().chain(unset) {
            cmd.arg(format!("--setenv={name}={}", value.to_string_lossy()));
        }
        let child = cmd.arg(sibling("legionctl-operator")).spawn().unwrap();
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
    /// `stdin` as its input.
    pub fn run(&self, cwd: &Path, env: &[(&str, &str)], args: &[&str], stdin: &str) -> Output {
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
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.held.clear();
    }
}
