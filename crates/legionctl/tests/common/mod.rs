//! The real operator on a test socket, started the way systemd starts it (socket activation), and the CLI against
//! it. The operator gets only the environment a test gives it; the D-Bus addresses point nowhere, so its session
//! module stops and machine changes cannot reach a daemon.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

pub struct Operator {
    /// Short on purpose: a unix socket path must fit in 108 bytes.
    pub runtime: PathBuf,
    child: Child,
}

impl Operator {
    pub fn start(tag: &str, env: &[(&str, OsString)]) -> Operator {
        let runtime = PathBuf::from(format!("/tmp/lc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&runtime);
        let socket = runtime.join("legionctl/api.sock");
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let nowhere = OsString::from("unix:path=/nonexistent/bus");
        let mut cmd = Command::new("systemd-socket-activate");
        cmd.env_clear().arg("-l").arg(&socket);
        for (name, value) in env.iter().chain(&[
            ("DBUS_SESSION_BUS_ADDRESS", nowhere.clone()),
            ("DBUS_SYSTEM_BUS_ADDRESS", nowhere),
            ("XDG_RUNTIME_DIR", runtime.clone().into()),
        ]) {
            cmd.arg(format!("--setenv={name}={}", value.to_string_lossy()));
        }
        // another package's binary: cargo builds it into the same folder (`make test` builds every binary first)
        let operator =
            Path::new(env!("CARGO_BIN_EXE_legionctl")).with_file_name("legionctl-operator");
        assert!(
            operator.exists(),
            "{} is missing: run `cargo build --workspace --bins`",
            operator.display()
        );
        let child = cmd.arg(operator).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "the test socket did not appear");
            std::thread::sleep(Duration::from_millis(20));
        }
        Operator { runtime, child }
    }

    /// `legionctl` with only `XDG_RUNTIME_DIR` (to find the socket), `PATH`, `env`, the working directory `cwd` and
    /// `stdin` as its input.
    pub fn run(&self, cwd: &Path, env: &[(&str, &str)], args: &[&str], stdin: &str) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_legionctl"))
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("XDG_RUNTIME_DIR", &self.runtime)
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
        let _ = std::fs::remove_dir_all(&self.runtime);
    }
}
