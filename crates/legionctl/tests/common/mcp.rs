//! `legionctl mcp` as an agent's harness runs it: JSON-RPC lines on its stdin and stdout.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use serde_json::{Value, json};

use crate::common::Operator;
use crate::common::process::Kill;

pub struct Mcp {
    /// The process the harness started: the server, or the agent that runs it.
    pub child: Kill,
    /// The server's output lines, read on a thread of their own: a reply that never comes fails the test (CI run
    /// 39631433941 waited 62 min on a server that had read no request).
    out: Receiver<String>,
    next: u64,
}

/// How long a test waits for one reply of the server.
const REPLY_WAIT: Duration = Duration::from_secs(300);

impl Mcp {
    /// `legionctl mcp <args>` started in the folder `cwd` (its calls' folder; none: the test's).
    pub fn start(
        operator: &Operator,
        cwd: Option<&std::path::Path>,
        env: &[(&str, &str)],
        args: &[&str],
    ) -> Mcp {
        let here = std::env::current_dir().unwrap();
        let cwd = cwd.unwrap_or(&here);
        Self::over(
            operator,
            env,
            Command::new(env!("CARGO_BIN_EXE_legionctl"))
                .arg("mcp")
                .args(args)
                .current_dir(cwd),
        )
    }

    /// `command` (an MCP server, or a process that runs one) with only `env` and the operator's runtime folder, as an
    /// agent's harness starts it, initialized.
    pub fn over(operator: &Operator, env: &[(&str, &str)], command: &mut Command) -> Mcp {
        let mut child = Kill::spawn(
            command
                .env_clear()
                .env("XDG_RUNTIME_DIR", &operator.runtime)
                .envs(env.iter().copied())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped()),
        );
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let (lines, out) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in stdout.lines().map_while(Result::ok) {
                if lines.send(line).is_err() {
                    break;
                }
            }
        });
        let mut mcp = Mcp {
            child,
            out,
            next: 0,
        };
        mcp.call(
            "initialize",
            &json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "0"}}),
        );
        mcp.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        mcp
    }

    pub fn send(&mut self, msg: &Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    }

    pub fn call(&mut self, method: &str, params: &Value) -> Value {
        self.call_raw(method, params)["result"].clone()
    }

    /// A call of the tool `name` with `arguments`: its text, and whether it is an error.
    pub fn tool(&mut self, name: &str, arguments: &Value) -> (String, bool) {
        let result = self.call("tools/call", &json!({"name": name, "arguments": arguments}));
        (
            result["content"][0]["text"].as_str().unwrap().to_string(),
            result["isError"] == true,
        )
    }

    /// `legionctl_run` of `op` with `input`: its JSON output or error envelope, and whether it is an error.
    pub fn run(&mut self, op: &str, input: &Value) -> (Value, bool) {
        let (text, error) = self.tool("legionctl_run", &json!({"op": op, "input": input}));
        (serde_json::from_str(&text).unwrap(), error)
    }

    /// The whole reply: its result or its error.
    pub fn call_raw(&mut self, method: &str, params: &Value) -> Value {
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params}));
        let line = match self.out.recv_timeout(REPLY_WAIT) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => {
                panic!(
                    "the MCP server gave no answer to {method} in {} s",
                    REPLY_WAIT.as_secs()
                )
            }
            Err(RecvTimeoutError::Disconnected) => {
                panic!("the MCP server ended its output before it answered {method}")
            }
        };
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], self.next, "{reply}");
        reply
    }
}
