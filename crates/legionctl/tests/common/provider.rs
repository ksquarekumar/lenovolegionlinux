//! The stand-in model provider for the tests that use the gateway (gateway.rs, e2e.rs).

use std::path::Path;
use std::process::Command;

pub use crate::common::process::Kill;

pub fn free_port() -> u16 {
    // the port is free again when this returns: two tests of one binary must not get the same one (a gateway that
    // cannot bind leaves its requests to the other test's gateway)
    static GIVEN: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());
    let mut given = GIVEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        if !given.contains(&port) {
            given.push(port);
            return port;
        }
    }
}

/// The stand-in model provider (`tests/fixtures/fake-provider.py`) on `port`, logging each call to `log`.
pub fn provider(port: u16, log: &Path) -> Kill {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-provider.py");
    Kill::spawn(
        Command::new("python3")
            .arg(fixture)
            .arg(port.to_string())
            .arg(log),
    )
}
