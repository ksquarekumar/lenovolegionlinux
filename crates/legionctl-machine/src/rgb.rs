//! Keyboard RGB through `OpenRGB`'s SDK server (TCP; `LEGIONCTL_OPENRGB`, default `127.0.0.1:6742`). `OpenRGB` 1.0
//! drives this keyboard (ITE 8258, USB `048d:c988`, its `LEGION_7GEN8`); legionctl loads the `OpenRGB` profile that the
//! configset knob `rgb.profile` wants, so the light follows the same conditions as the power policy.
//!
//! The SDK packet: `ORGB`, then the device index, the packet id and the data size (u32, little endian), then the data.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};

use crate::sysroot::SysRoot;

/// The keyboard's USB vendor and product id.
pub const KEYBOARD: (&str, &str) = ("048d", "c988");
const DEFAULT: &str = "127.0.0.1:6742";
/// The highest SDK protocol this client speaks; profiles need 2.
const OURS: u32 = 4;
const CONTROLLER_COUNT: u32 = 0;
const CONTROLLER_DATA: u32 = 1;
const PROTOCOL_VERSION: u32 = 40;
const PROFILE_LIST: u32 = 150;
const LOAD_PROFILE: u32 = 152;
/// A reply is a few KiB; a larger size is not a reply of `OpenRGB`.
const MAX_REPLY: u32 = 1 << 20;
/// The time of one connection, from connect to the last reply: the root daemon loads a profile on its event loop, so
/// a server (any local user can listen on the port) must not hold it longer.
const DEADLINE: Duration = Duration::from_secs(2);

/// Where `OpenRGB`'s SDK server answers.
#[must_use]
pub fn address() -> String {
    std::env::var("LEGIONCTL_OPENRGB").unwrap_or_else(|_| DEFAULT.into())
}

/// Whether the keyboard is on the USB bus.
#[must_use]
pub fn keyboard_present(root: &SysRoot) -> bool {
    root.children("/sys/bus/usb/devices", "").iter().any(|d| {
        root.read(d.join("idVendor")).as_deref() == Some(KEYBOARD.0)
            && root.read(d.join("idProduct")).as_deref() == Some(KEYBOARD.1)
    })
}

/// A connection to `OpenRGB`'s SDK server.
pub struct Client {
    stream: TcpStream,
    deadline: Instant,
    /// The protocol both sides speak.
    pub protocol: u32,
}

impl Client {
    /// Connect and agree on the protocol.
    ///
    /// # Errors
    /// No server answers at `address`, it does not speak the SDK protocol, or it takes longer than [`DEADLINE`].
    pub fn connect(address: &str) -> Result<Client> {
        let at = address
            .to_socket_addrs()
            .ok()
            .and_then(|mut a| a.next())
            .with_context(|| format!("{address} is not an address"))?;
        let deadline = Instant::now() + DEADLINE;
        let stream = TcpStream::connect_timeout(&at, DEADLINE).with_context(|| {
            format!("OpenRGB does not answer at {address}: start it with its SDK server (openrgb --server)")
        })?;
        let mut client = Client {
            stream,
            deadline,
            protocol: 0,
        };
        client.send(0, PROTOCOL_VERSION, &OURS.to_le_bytes())?;
        let reply = client.reply(PROTOCOL_VERSION)?;
        client.protocol = u32_at(&reply, 0)?.min(OURS);
        Ok(client)
    }

    fn send(&mut self, device: u32, id: u32, data: &[u8]) -> Result<()> {
        let mut packet = b"ORGB".to_vec();
        for n in [device, id, u32::try_from(data.len())?] {
            packet.extend(n.to_le_bytes());
        }
        packet.extend(data);
        self.stream.set_write_timeout(Some(self.left()?))?;
        self.stream.write_all(&packet).context("writing to OpenRGB")
    }

    /// The time left before the deadline.
    fn left(&self) -> Result<Duration> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        ensure!(
            !left.is_zero(),
            "OpenRGB did not answer within {} s",
            DEADLINE.as_secs()
        );
        Ok(left)
    }

    /// Fill `buf`, each read within the time left (a server that sends a byte at a time cannot hold it longer).
    fn fill(&mut self, buf: &mut [u8]) -> Result<()> {
        let mut at = 0;
        while at < buf.len() {
            self.stream.set_read_timeout(Some(self.left()?))?;
            match self.stream.read(&mut buf[at..]) {
                Ok(0) => bail!("OpenRGB closed the connection"),
                Ok(n) => at += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e).context("OpenRGB did not answer"),
            }
        }
        Ok(())
    }

    /// The data of the next packet `id`; other packets (device list notices) are skipped until the deadline.
    fn reply(&mut self, id: u32) -> Result<Vec<u8>> {
        loop {
            let mut header = [0u8; 16];
            self.fill(&mut header)?;
            ensure!(&header[..4] == b"ORGB", "the server is not OpenRGB");
            let size = u32_at(&header, 12)?;
            ensure!(size <= MAX_REPLY, "OpenRGB sent {size} bytes");
            let mut data = vec![0u8; usize::try_from(size)?];
            self.fill(&mut data)?;
            if u32_at(&header, 8)? == id {
                return Ok(data);
            }
        }
    }

    /// The names of the devices `OpenRGB` drives.
    ///
    /// # Errors
    /// The server does not answer as the protocol says.
    pub fn controllers(&mut self) -> Result<Vec<String>> {
        self.send(0, CONTROLLER_COUNT, &[])?;
        let count = u32_at(&self.reply(CONTROLLER_COUNT)?, 0)?;
        let mut names = Vec::new();
        for index in 0..count {
            let request = if self.protocol == 0 {
                Vec::new()
            } else {
                self.protocol.to_le_bytes().to_vec()
            };
            self.send(index, CONTROLLER_DATA, &request)?;
            // data size (u32), device type (i32), then the name
            names.push(string_at(&self.reply(CONTROLLER_DATA)?, 8)?.0);
        }
        Ok(names)
    }

    /// The profiles `OpenRGB` has saved.
    ///
    /// # Errors
    /// The server's protocol has no profiles, or it does not answer as the protocol says.
    pub fn profiles(&mut self) -> Result<Vec<String>> {
        ensure!(
            self.protocol >= 2,
            "OpenRGB speaks SDK protocol {}; profiles need protocol 2 (OpenRGB 0.6 or later)",
            self.protocol
        );
        self.send(0, PROFILE_LIST, &[])?;
        let data = self.reply(PROFILE_LIST)?;
        // data size (u32), count (u16), then each name
        let count = u16::from_le_bytes(data.get(4..6).context("a short profile list")?.try_into()?);
        let mut at = 6;
        let mut names = Vec::new();
        for _ in 0..count {
            let (name, next) = string_at(&data, at)?;
            names.push(name);
            at = next;
        }
        Ok(names)
    }

    /// Load a saved profile. `OpenRGB` does not answer a load, and ignores an unknown name, so the name is checked
    /// against its list first.
    ///
    /// # Errors
    /// `OpenRGB` has no profile `name`, or the server does not answer as the protocol says.
    pub fn load(&mut self, name: &str) -> Result<()> {
        let profiles = self.profiles()?;
        if !profiles.iter().any(|p| p == name) {
            bail!(
                "OpenRGB has no profile {name} (it has: {}); save it in OpenRGB first",
                profiles.join(", ")
            );
        }
        let mut data = name.as_bytes().to_vec();
        data.push(0);
        self.send(0, LOAD_PROFILE, &data)
    }
}

fn u32_at(data: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        data.get(at..at + 4)
            .context("a short OpenRGB packet")?
            .try_into()?,
    ))
}

/// A string at `at` (u16 length with its end mark, then the bytes) and the offset after it.
fn string_at(data: &[u8], at: usize) -> Result<(String, usize)> {
    let len = usize::from(u16::from_le_bytes(
        data.get(at..at + 2)
            .context("a short OpenRGB packet")?
            .try_into()?,
    ));
    let bytes = data
        .get(at + 2..at + 2 + len)
        .context("a short OpenRGB packet")?;
    let text = String::from_utf8_lossy(bytes)
        .trim_end_matches('\0')
        .to_string();
    Ok((text, at + 2 + len))
}

/// Load `profile` through the server at [`address`].
///
/// # Errors
/// See [`Client::connect`] and [`Client::load`].
pub fn load(profile: &str) -> Result<()> {
    Client::connect(&address())?.load(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A local server on the port that never answers the request (R3-6): it sends notices one after the other, or one
    /// byte at a time. A load ends with an error within the deadline either way, so the root daemon's event loop goes
    /// on; before the fix the client waited for ever.
    #[test]
    fn a_server_that_never_replies_cannot_hold_a_load() {
        for trickle in [false, true] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap().to_string();
            std::thread::spawn(move || {
                let (mut s, _) = listener.accept().unwrap();
                let mut notice = b"ORGB".to_vec();
                for n in [0u32, 100, 0] {
                    notice.extend(n.to_le_bytes());
                }
                loop {
                    let sent = if trickle {
                        notice.iter().try_for_each(|b| {
                            std::thread::sleep(Duration::from_millis(100));
                            s.write_all(&[*b])
                        })
                    } else {
                        std::thread::sleep(Duration::from_millis(50));
                        s.write_all(&notice)
                    };
                    if sent.is_err() {
                        return;
                    }
                }
            });
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let started = Instant::now();
                let out = Client::connect(&address).and_then(|mut c| c.load("battery-dim"));
                tx.send((out.map_err(|e| format!("{e:#}")), started.elapsed()))
                    .unwrap();
            });
            let (out, took) = rx
                .recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|_| panic!("trickle {trickle}: the load still waits after 10 s"));
            assert!(out.is_err(), "trickle {trickle}: {out:?}");
            assert!(took < Duration::from_secs(4), "trickle {trickle}: {took:?}");
        }
    }
}
