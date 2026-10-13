//! A read-only reader of Windows registry hive files (regf), enough for the dual-boot checks (note 05): keys by path
//! and DWORD values. Every offset in the file is checked, so a damaged hive is an error, not a panic.
//!
//! shortcut: the transaction logs (`SYSTEM.LOG1`/`LOG2`) are not replayed, so a value written in the last Windows
//! session before a hard stop can be missing; replay them when a check depends on such a value.

use std::path::Path;

use anyhow::{Context, Result, bail};

/// Hive bins start after the 4 KiB base block; cell offsets count from there.
const BINS: usize = 4096;
/// `nk` flag: the key name is Latin-1, else UTF-16LE.
const KEY_COMP_NAME: u16 = 0x20;
/// `vk` flag: the value name is Latin-1, else UTF-16LE.
const VALUE_COMP_NAME: u16 = 0x1;
const REG_DWORD: u32 = 4;

pub struct Hive {
    data: Vec<u8>,
}

impl Hive {
    /// # Errors
    /// The file cannot be read or is not a hive.
    pub fn open(path: &Path) -> Result<Hive> {
        let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        if data.get(..4) != Some(b"regf") || data.len() < BINS {
            bail!("{} is not a registry hive", path.display());
        }
        Ok(Hive { data })
    }

    fn u16(&self, at: usize) -> Result<u16> {
        let b = self.data.get(at..at + 2).context("the hive ends early")?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&self, at: usize) -> Result<u32> {
        let b = self.data.get(at..at + 4).context("the hive ends early")?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Where the data of the cell at `offset` starts (after its size).
    fn cell(&self, offset: u32) -> Result<usize> {
        let at = BINS + usize::try_from(offset)? + 4;
        if at + 2 > self.data.len() {
            bail!("a cell offset ({offset:#x}) is outside the hive");
        }
        Ok(at)
    }

    fn signed(&self, at: usize, signature: [u8; 2]) -> Result<usize> {
        if self.data.get(at..at + 2) != Some(&signature) {
            bail!(
                "expected a {} cell at {at:#x}",
                String::from_utf8_lossy(&signature)
            );
        }
        Ok(at)
    }

    fn name(&self, at: usize, len: u16, latin1: bool) -> Result<String> {
        let bytes = self
            .data
            .get(at..at + usize::from(len))
            .context("a name is outside the hive")?;
        Ok(if latin1 {
            bytes.iter().map(|b| char::from(*b)).collect()
        } else {
            let units: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect();
            String::from_utf16_lossy(&units)
        })
    }

    /// The key at `path` (names split by `\`, any case), from the root key.
    fn key(&self, path: &str) -> Result<Option<usize>> {
        let mut key = self.signed(self.cell(self.u32(0x24)?)?, *b"nk")?;
        for part in path.split('\\').filter(|p| !p.is_empty()) {
            let count = self.u32(key + 20)?;
            if count == 0 {
                return Ok(None);
            }
            match self.subkey(self.cell(self.u32(key + 28)?)?, part, 0)? {
                Some(found) => key = found,
                None => return Ok(None),
            }
        }
        Ok(Some(key))
    }

    /// The child named `name` in the subkey list at `list` (`lf`, `lh`, `li`, or `ri` of those).
    fn subkey(&self, list: usize, name: &str, depth: u8) -> Result<Option<usize>> {
        let kind = self
            .data
            .get(list..list + 2)
            .context("a list is outside the hive")?;
        let count = usize::from(self.u16(list + 2)?);
        let (step, nested) = match kind {
            b"lf" | b"lh" => (8, false),
            b"li" => (4, false),
            b"ri" if depth == 0 => (4, true),
            _ => bail!("unknown subkey list at {list:#x}"),
        };
        for i in 0..count {
            let at = self.cell(self.u32(list + 4 + i * step)?)?;
            if nested {
                if let Some(found) = self.subkey(at, name, depth + 1)? {
                    return Ok(Some(found));
                }
                continue;
            }
            let child = self.signed(at, *b"nk")?;
            let flags = self.u16(child + 2)?;
            let own = self.name(
                child + 76,
                self.u16(child + 72)?,
                flags & KEY_COMP_NAME != 0,
            )?;
            if own.to_lowercase() == name.to_lowercase() {
                return Ok(Some(child));
            }
        }
        Ok(None)
    }

    /// The DWORD `value` of the key at `path`: None when the key or the value is missing.
    ///
    /// # Errors
    /// The hive is damaged, or the value is not a DWORD.
    pub fn dword(&self, path: &str, value: &str) -> Result<Option<u32>> {
        let Some(key) = self.key(path)? else {
            return Ok(None);
        };
        let count = self.u32(key + 36)?;
        if count == 0 {
            return Ok(None);
        }
        let list = self.cell(self.u32(key + 40)?)?;
        for i in 0..usize::try_from(count)? {
            let vk = self.signed(self.cell(self.u32(list + i * 4)?)?, *b"vk")?;
            let flags = self.u16(vk + 16)?;
            let own = self.name(vk + 20, self.u16(vk + 2)?, flags & VALUE_COMP_NAME != 0)?;
            if own.to_lowercase() != value.to_lowercase() {
                continue;
            }
            let (size, kind) = (self.u32(vk + 4)?, self.u32(vk + 12)?);
            if kind != REG_DWORD || size & 0x7fff_ffff != 4 {
                bail!("{path}\\{value} is not a DWORD (type {kind}, {size:#x} bytes)");
            }
            // a set high bit keeps the data in the offset field itself
            let at = if size & 0x8000_0000 == 0 {
                self.cell(self.u32(vk + 8)?)?
            } else {
                vk + 8
            };
            return Ok(Some(self.u32(at)?));
        }
        Ok(None)
    }

    /// `ControlSet00N`, the control set Windows starts with (`Select\Current`); `CurrentControlSet` is a link to it.
    ///
    /// # Errors
    /// The hive is damaged or has no `Select\Current`.
    pub fn current_control_set(&self) -> Result<String> {
        let current = self
            .dword("Select", "Current")?
            .context("the hive has no Select\\Current: not a SYSTEM hive")?;
        Ok(format!("ControlSet{current:03}"))
    }
}
