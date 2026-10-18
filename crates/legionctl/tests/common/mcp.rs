//! `legionctl mcp` as an agent's harness runs it: JSON-RPC lines on its stdin and stdout.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use crate::common::Operator;
use crate::common::process::Kill;

pub struct Mcp {
    child: Kill,
    out: BufReader<std::process::ChildStdout>,
    next: u64,
}

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
        let mut child = Kill::spawn(
            Command::new(env!("CARGO_BIN_EXE_legionctl"))
                .arg("mcp")
                .args(args)
                .current_dir(cwd)
                .env_clear()
                .env("XDG_RUNTIME_DIR", &operator.runtime)
                .envs(env.iter().copied())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped()),
        );
        let out = BufReader::new(child.stdout.take().unwrap());
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
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], self.next, "{reply}");
        reply
    }
}
