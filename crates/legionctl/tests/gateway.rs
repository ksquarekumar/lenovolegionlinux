//! The model gateway in the operator, in front of a stand-in provider (`tests/fixtures/fake-provider.py`): the pool
//! moves past a rate-limited account, the caller's own credentials go through, exact answers replay (streams too),
//! an explicit account gets its 429, a money budget stops an account (and needs a price), a keyring account needs
//! the gateway token, and translating between the two APIs is refused.

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use common::Operator;
use serde_json::{Value, json};

struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn wait_for(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "nothing listens on {port}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// One HTTP/1.1 POST; returns status, the `x-legionctl-cache` header and the body.
fn post(port: u16, path: &str, headers: &[(&str, &str)], body: &Value) -> (u16, String, String) {
    let body = body.to_string();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\ncontent-type: application/json\r\ncontent-length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        let _ = std::fmt::Write::write_fmt(&mut request, format_args!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(&body);
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let response = String::from_utf8_lossy(&response).to_string();
    let (head, rest) = response.split_once("\r\n\r\n").unwrap();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    let cache = head
        .lines()
        .find_map(|l| {
            l.to_lowercase()
                .strip_prefix("x-legionctl-cache: ")
                .map(String::from)
        })
        .unwrap_or_default();
    // a chunked body: keep only the chunk data
    let body = if head.to_lowercase().contains("transfer-encoding: chunked") {
        let mut out = String::new();
        let mut rest = rest;
        while let Some((size, after)) = rest.split_once("\r\n") {
            let size = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
            if size == 0 {
                break;
            }
            out.push_str(&after[..size]);
            rest = &after[size + 2..];
        }
        out
    } else {
        rest.to_string()
    };
    (status, cache, body)
}

fn provider_calls(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

/// The operator with its gateway, in front of the stand-in provider at /ok and /limited.
struct Setup {
    _provider: Kill,
    op: Operator,
    home: PathBuf,
    port: u16,
    log: PathBuf,
}

fn setup(tag: &str) -> Setup {
    let home = PathBuf::from(format!(
        "/var/tmp/legionctl-gateway-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".config/legionctl")).unwrap();
    let (provider_port, port) = (free_port(), free_port());
    let log = home.join("provider.log");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-provider.py");
    let provider = Kill(
        Command::new("python3")
            .arg(fixture)
            .arg(provider_port.to_string())
            .arg(&log)
            .spawn()
            .unwrap(),
    );
    let base = |prefix: &str| format!("http://127.0.0.1:{provider_port}/{prefix}");
    std::fs::write(
        home.join(".config/legionctl/gateway.jsonc"),
        json!({"gateway": {
            "port": port,
            "accounts": {
                "limited": {"provider": "anthropic", "base_url": base("limited")},
                "main": {"provider": "anthropic", "base_url": base("ok")},
                "keyed": {"provider": "anthropic", "base_url": base("ok"), "keyring": true}
            },
            "pool": {"claude-x": ["limited", "main"], "*": ["main"]},
            // 10 tokens in and 5 out at these prices cost $2: one call uses the day
            "prices": {"claude-y": {"input": 100_000.0, "output": 200_000.0}},
            "budgets": {"main/claude-y": {"day_usd": 2.0}}
        }})
        .to_string(),
    )
    .unwrap();
    let op = Operator::start(
        tag,
        &[
            ("HOME", home.clone().into()),
            ("PATH", std::env::var_os("PATH").unwrap()),
        ],
    );
    op.ok(&["load"]);
    wait_for(provider_port);
    wait_for(port);
    Setup {
        _provider: provider,
        op,
        home,
        port,
        log,
    }
}

const CALLER: [(&str, &str); 2] = [
    ("x-api-key", "caller-key"),
    ("anthropic-version", "2023-06-01"),
];

fn ask(model: &str, text: &str) -> Value {
    json!({"model": model, "max_tokens": 10, "messages": [{"role": "user", "content": text}]})
}

#[test]
fn the_gateway_pools_forwards_replays_and_streams() {
    let s = setup("gw-pool");
    let messages =
        |headers: &[(&str, &str)], body: &Value| post(s.port, "/v1/messages", headers, body);
    // the pool: limited answers 429, main serves; the caller's key goes through
    let (status, cache, body) = messages(&CALLER, &ask("claude-x", "hi"));
    assert_eq!((status, cache.as_str()), (200, "miss"), "{body}");
    assert!(body.contains("model=claude-x key=caller-key"), "{body}");
    assert_eq!(
        provider_calls(&s.log),
        ["limited claude-x caller-key", "ok claude-x caller-key"]
    );
    // the same request replays; refresh calls again
    assert_eq!(
        messages(&CALLER, &ask("claude-x", "hi")),
        (200, "hit".into(), body)
    );
    let refresh = [CALLER[0], CALLER[1], ("x-legionctl-cache", "refresh")];
    assert_eq!(messages(&refresh, &ask("claude-x", "hi")).1, "miss");
    assert_eq!(
        provider_calls(&s.log).len(),
        4,
        "refresh called the pool again"
    );
    // a stream passes through and replays byte for byte
    let mut stream = ask("claude-z", "hi");
    stream["stream"] = json!(true);
    let (status, _, streamed) = messages(&CALLER, &stream);
    assert!(
        status == 200 && streamed.contains("event: message_delta"),
        "{streamed}"
    );
    assert_eq!(messages(&CALLER, &stream), (200, "hit".into(), streamed));

    let usage: Value = serde_json::from_str(&s.op.ok(&["gateway", "usage", "--json"])).unwrap();
    let rows: Vec<(String, bool, u64, i64)> = usage
        .as_array()
        .unwrap()
        .iter()
        .map(|u| {
            let account = u["account"].as_str().unwrap().into();
            (
                account,
                u["cached"].as_bool().unwrap(),
                u["status"].as_u64().unwrap(),
                u["tokens_out"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("limited".into(), false, 429, 0),
            ("main".into(), false, 200, 5),
            ("cache".into(), true, 200, 5),
            ("limited".into(), false, 429, 0),
            ("main".into(), false, 200, 5),
            ("main".into(), false, 200, 5),
            ("cache".into(), true, 200, 5),
        ]
    );
}

#[test]
fn the_gateway_refuses_over_budget_without_token_and_across_apis() {
    let s = setup("gw-refuse");
    let messages = |body: &Value| post(s.port, "/v1/messages", &CALLER, body);
    // an explicit account gets its own 429; no pool
    assert_eq!(messages(&ask("limited/claude-x", "hi")).0, 429);
    // a day's budget of $2: one call costs it; a new question is refused
    assert_eq!(messages(&ask("main/claude-y", "one")).0, 200);
    let (status, _, body) = messages(&ask("main/claude-y", "two"));
    assert!(
        status == 429 && body.contains("budget of $2.00 per day is used ($2.00)"),
        "{body}"
    );
    // a keyring account needs the gateway token; the other API needs a translation that is not built
    let (status, _, body) = messages(&ask("keyed/claude-x", "hi"));
    assert!(status == 401 && body.contains("gateway token"), "{body}");
    let (status, _, body) = post(
        s.port,
        "/v1/chat/completions",
        &CALLER,
        &ask("main/claude-x", "hi"),
    );
    assert!(
        status == 400 && body.contains("translating between the OpenAI and Anthropic APIs"),
        "{body}"
    );
    assert_eq!(
        provider_calls(&s.log),
        ["limited claude-x caller-key", "ok claude-y caller-key"]
    );
    let usage: Value = serde_json::from_str(&s.op.ok(&["gateway", "usage", "--json"])).unwrap();
    let costs: Vec<f64> = usage
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["cost_usd"].as_f64().unwrap())
        .collect();
    assert_eq!(
        costs,
        [0.0, 2.0],
        "the 429 costs nothing; the call costs its tokens"
    );
}

#[test]
fn a_budget_without_a_price_is_refused_with_the_reason() {
    let s = setup("gw-price");
    let config = s.home.join(".config/legionctl/gateway.jsonc");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        &config,
        text.replace(r#""prices":{"claude-y""#, r#""prices":{"claude-z""#),
    )
    .unwrap();
    let out = s.op.run(&s.home, &[], &["config", "show"], "");
    assert!(
        !out.status.success()
            && String::from_utf8_lossy(&out.stderr)
                .contains("gateway.budgets.main/claude-y: gateway.prices has no claude-y"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
