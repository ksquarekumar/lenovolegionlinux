//! I/O latency per disk (note 04): loads `bpf/io.bpf.c` and reads its log2 histogram per disk.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use aya::maps::{HashMap, MapData};
use legionctl_machine::sysroot::SysRoot;

pub struct Block {
    hist: HashMap<MapData, u32, [u64; 32]>,
}

/// The kernel's name of disk `dev` (major << 20 | minor): the program's ids are the running kernel's, so its sysfs
/// names them; a disk that went away keeps its numbers.
fn disk(dev: u32) -> String {
    let (major, minor) = (dev >> 20, dev & 0xf_ffff);
    std::fs::read_link(format!("/sys/dev/block/{major}:{minor}"))
        .ok()
        .and_then(|p| Some(p.file_name()?.to_string_lossy().into_owned()))
        .unwrap_or_else(|| format!("{major}:{minor}"))
}

impl Block {
    /// The pinned programs of this build, else load, attach and pin them.
    ///
    /// # Errors
    /// No BPF permission, no BTF, the verifier refuses a program (its message is in the error), or a pin fails.
    pub fn start(root: &SysRoot) -> Result<Block> {
        let object = aya::include_bytes_aligned!(concat!(env!("OUT_DIR"), "/io.bpf.o"));
        let [hist] = crate::pins::attach(
            root,
            "io",
            object,
            &[
                ("issue", "block_rq_issue"),
                ("complete", "block_rq_complete"),
            ],
            &["io_hist"],
        )?
        .try_into()
        .ok()
        .context("one map")?;
        Ok(Block {
            hist: HashMap::try_from(hist)?,
        })
    }

    /// The cumulative histogram of each disk (bucket i: 2^i to 2^(i+1) µs).
    ///
    /// # Errors
    /// The map cannot be read.
    pub fn per_disk(&self) -> Result<BTreeMap<String, Vec<u64>>> {
        self.hist
            .iter()
            .map(|entry| {
                let (dev, hist) = entry?;
                Ok((disk(dev), hist.to_vec()))
            })
            .collect()
    }
}
