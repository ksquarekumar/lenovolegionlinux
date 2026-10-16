//! The QEMU guest agent (`qemu-ga` in Windows, from the virtio-win guest tools) over the Windows VM's unix socket:
//! programs run in the running Windows, their output read back from a file while they run (todo-151).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine;
use serde_json::{Value, json};

/// A guest agent connection after `guest-sync`.
pub struct Qga {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Qga {
    /// # Errors
    /// No VM listens at `socket`, or the guest agent does not answer in 10 s.
    pub fn connect(socket: &Path) -> Result<Qga> {
        let stream = UnixStream::connect(socket).with_context(|| {
            format!(
                "the Windows VM's guest agent socket {} does not answer",
                socket.display()
            )
        })?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut qga = Qga {
            reader: BufReader::new(stream.try_clone()?),
            stream,
        };
        // answers of an earlier client can still wait in the channel: read up to the answer with our id
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| u64::from(d.subsec_nanos()));
        writeln!(
            qga.stream,
            "{}",
            json!({"execute": "guest-sync", "arguments": {"id": id}})
        )?;
        loop {
            let reply = qga.read().context(
                "the QEMU guest agent in Windows does not answer in 10 s: Windows does not run (or runs WinRE), or \
                 the virtio-win guest tools are not installed",
            )?;
            if reply["return"] == json!(id) {
                return Ok(qga);
            }
        }
    }

    fn read(&mut self) -> Result<Value> {
        let mut line = String::new();
        anyhow::ensure!(
            self.reader.read_line(&mut line)? > 0,
            "the guest agent closed the channel"
        );
        Ok(serde_json::from_str(line.trim_start_matches('\u{ff}'))?)
    }

    /// Run `command` in the guest and return its result.
    ///
    /// # Errors
    /// The agent refuses the command or does not answer in 10 s.
    pub fn execute(&mut self, command: &str, arguments: &Value) -> Result<Value> {
        writeln!(
            self.stream,
            "{}",
            json!({ "execute": command, "arguments": arguments })
        )?;
        let reply = self
            .read()
            .with_context(|| format!("the guest agent did not answer {command}"))?;
        if let Some(error) = reply.get("error") {
            anyhow::bail!("the guest agent refused {command}: {}", error["desc"]);
        }
        Ok(reply["return"].clone())
    }

    /// Start `path` with `args` in the guest; its pid.
    ///
    /// # Errors
    /// The agent cannot start it.
    pub fn spawn(&mut self, path: &str, args: &[String]) -> Result<i64> {
        self.execute("guest-exec", &json!({"path": path, "arg": args}))?["pid"]
            .as_i64()
            .context("guest-exec gave no pid")
    }

    /// The exit code of `pid`, or None while it runs.
    ///
    /// # Errors
    /// The agent does not know the process.
    pub fn exited(&mut self, pid: i64) -> Result<Option<i64>> {
        let status = self.execute("guest-exec-status", &json!({ "pid": pid }))?;
        Ok((status["exited"] == true).then(|| status["exitcode"].as_i64().unwrap_or(-1)))
    }

    /// The whole file `path` of the guest.
    ///
    /// # Errors
    /// The file cannot be opened or read.
    pub fn read_file(&mut self, path: &str) -> Result<Vec<u8>> {
        let handle = self.execute("guest-file-open", &json!({"path": path, "mode": "rb"}))?;
        let mut out = Vec::new();
        let read = (|| -> Result<()> {
            loop {
                let chunk = self.execute(
                    "guest-file-read",
                    &json!({"handle": handle, "count": 65536}),
                )?;
                out.extend(
                    base64::engine::general_purpose::STANDARD
                        .decode(chunk["buf-b64"].as_str().unwrap_or_default())?,
                );
                if chunk["eof"] == true {
                    return Ok(());
                }
            }
        })();
        self.execute("guest-file-close", &json!({ "handle": handle }))?;
        read.map(|()| out)
    }

    /// Run `path` with the arguments `args` gives for a new log file (they write the program's output there; the
    /// agent runs as SYSTEM) until it ends; `chunk` gets the new output after each poll. Returns the exit code and the
    /// whole output.
    ///
    /// # Errors
    /// The agent stops answering (Windows stopped or restarted) or a `chunk` fails.
    pub fn run(
        &mut self,
        path: &str,
        args: impl FnOnce(&str) -> Vec<String>,
        mut chunk: impl FnMut(&str) -> Result<()>,
    ) -> Result<(i64, String)> {
        let log = format!(
            r"C:\Windows\Temp\legionctl-{}.log",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        );
        let pid = self.spawn(path, &args(&log))?;
        let mut shown = 0;
        loop {
            let code = self.exited(pid)?;
            // the program makes the file; until then there is no output
            let text = match self.read_file(&log) {
                Ok(bytes) => text(&bytes),
                Err(_) if code.is_none() => String::new(),
                Err(e) => return Err(e),
            };
            if let Some(new) = text.get(shown..).filter(|n| !n.is_empty()) {
                chunk(new)?;
                shown = text.len();
            }
            if let Some(code) = code {
                return Ok((code, text));
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

/// A program's output as text: UTF-16 (`sfc` writes it) or UTF-8 and the OEM code page's ASCII; without a BOM and
/// carriage returns (DISM draws its progress bar with them).
#[must_use]
pub fn text(bytes: &[u8]) -> String {
    let utf16 = bytes.len() >= 2
        && bytes.iter().skip(1).step_by(2).filter(|b| **b == 0).count() * 2 >= bytes.len() / 2;
    let text = if utf16 {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    text.trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}
