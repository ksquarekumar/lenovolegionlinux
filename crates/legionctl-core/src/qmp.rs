//! QEMU's machine protocol (QMP) over a unix socket: the Windows VM's power button, its end, and GPU hot-plug.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};

/// A QMP connection after the capabilities handshake.
pub struct Qmp {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Qmp {
    /// # Errors
    /// No QEMU listens at `socket`, or it does not speak QMP.
    pub fn connect(socket: &Path) -> Result<Qmp> {
        let stream = UnixStream::connect(socket)
            .with_context(|| format!("connecting to {}", socket.display()))?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        let mut qmp = Qmp {
            reader: BufReader::new(stream.try_clone()?),
            stream,
        };
        qmp.read()?;
        qmp.execute("qmp_capabilities", None)?;
        Ok(qmp)
    }

    fn read(&mut self) -> Result<Value> {
        let mut line = String::new();
        anyhow::ensure!(self.reader.read_line(&mut line)? > 0, "QEMU closed QMP");
        Ok(serde_json::from_str(&line)?)
    }

    /// Run `command` and return its result; events that come first are skipped.
    ///
    /// # Errors
    /// QEMU refuses the command or closes the connection.
    pub fn execute(&mut self, command: &str, arguments: Option<Value>) -> Result<Value> {
        let mut message = json!({ "execute": command });
        if let Some(arguments) = arguments {
            message["arguments"] = arguments;
        }
        writeln!(self.stream, "{message}")?;
        loop {
            let reply = self.read()?;
            if let Some(error) = reply.get("error") {
                anyhow::bail!("QEMU refused {command}: {}", error["desc"]);
            }
            if let Some(result) = reply.get("return") {
                return Ok(result.clone());
            }
        }
    }

    /// Wait until QEMU sends `event` whose `data.device` is `device`, for at most `limit`.
    ///
    /// # Errors
    /// The event does not come in time, or QEMU closes the connection.
    pub fn wait_event(&mut self, event: &str, device: &str, limit: Duration) -> Result<()> {
        let deadline = std::time::Instant::now() + limit;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            anyhow::ensure!(
                !left.is_zero(),
                "no {event} for {device} in {} s",
                limit.as_secs()
            );
            self.stream.set_read_timeout(Some(left))?;
            let message = self.read()?;
            if message["event"] == event && message["data"]["device"] == device {
                return Ok(());
            }
        }
    }
}
