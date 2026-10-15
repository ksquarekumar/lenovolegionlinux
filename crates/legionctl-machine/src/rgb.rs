//! Keyboard RGB through `OpenRGB`'s SDK server (TCP; `LEGIONCTL_OPENRGB`, default `127.0.0.1:6742`). `OpenRGB` 1.0
//! drives this keyboard (ITE 8258, USB `048d:c988`, its `LEGION_7GEN8`); legionctl loads the `OpenRGB` profile that the
//! configset knob `rgb.profile` wants, so the light follows the same conditions as the power policy.
//!
//! The SDK packet: `ORGB`, then the device index, the packet id and the data size (u32, little endian), then the data.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

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
    /// The protocol both sides speak.
    pub protocol: u32,
}

impl Client {
    /// Connect and agree on the protocol.
    ///
    /// # Errors
    /// No server answers at `address`, or it does not speak the SDK protocol.
    pub fn connect(address: &str) -> Result<Client> {
        let at = address
            .to_socket_addrs()
            .ok()
            .and_then(|mut a| a.next())
            .with_context(|| format!("{address} is not an address"))?;
        let stream = TcpStream::connect_timeout(&at, Duration::from_secs(1)).with_context(|| {
            format!("OpenRGB does not answer at {address}: start it with its SDK server (openrgb --server)")
        })?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut client = Client {
            stream,
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
        self.stream.write_all(&packet).context("writing to OpenRGB")
    }

    /// The data of the next packet `id`; other packets (device list notices) are skipped.
    fn reply(&mut self, id: u32) -> Result<Vec<u8>> {
        loop {
            let mut header = [0u8; 16];
            self.stream
                .read_exact(&mut header)
                .context("OpenRGB did not answer")?;
            ensure!(&header[..4] == b"ORGB", "the server is not OpenRGB");
            let size = u32_at(&header, 12)?;
            ensure!(size <= MAX_REPLY, "OpenRGB sent {size} bytes");
            let mut data = vec![0u8; usize::try_from(size)?];
            self.stream.read_exact(&mut data)?;
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
