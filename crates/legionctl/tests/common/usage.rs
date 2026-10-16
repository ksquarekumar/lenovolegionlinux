//! The stand-in usage endpoint of both providers for the account tests (todo-136): `tests/fixtures/fake-usage.py`.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::common::process::Kill;

/// The stand-in usage endpoint, answering from the files in `dir` (`<token>.json`), and its port.
pub fn usage_server(dir: &Path) -> (Kill, u16) {
    std::fs::create_dir_all(dir).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-usage.py");
    let server = Kill::spawn(Command::new("python3").arg(fixture).arg(dir));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(port) = std::fs::read_to_string(dir.join("port")) {
            return (server, port.trim().parse().unwrap());
        }
        assert!(
            Instant::now() < deadline,
            "the usage stand-in did not start"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
