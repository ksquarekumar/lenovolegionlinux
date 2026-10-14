//! The stand-in model provider for the tests that use the gateway (gateway.rs, e2e.rs).

use std::path::Path;
use std::process::{Child, Command};

/// A child process that ends with the test.
pub struct Kill(pub Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The stand-in model provider (`tests/fixtures/fake-provider.py`) on `port`, logging each call to `log`.
pub fn provider(port: u16, log: &Path) -> Kill {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-provider.py");
    Kill(
        Command::new("python3")
            .arg(fixture)
            .arg(port.to_string())
            .arg(log)
            .spawn()
            .unwrap(),
    )
}
