//! The run-queue latency sensor (note 04): loads `bpf/runq.bpf.c` and reads its per-CPU log2 histogram.

use anyhow::{Context, Result};
use aya::maps::{MapData, PerCpuArray};
use legionctl_machine::sysroot::SysRoot;

pub struct Runq {
    hist: PerCpuArray<MapData, u64>,
}

impl Runq {
    /// The pinned programs of this build, else load, attach and pin them.
    ///
    /// # Errors
    /// No BPF permission, no BTF, the verifier refuses a program (its message is in the error), or a pin fails.
    pub fn start(root: &SysRoot) -> Result<Runq> {
        let object = aya::include_bytes_aligned!(concat!(env!("OUT_DIR"), "/runq.bpf.o"));
        let [hist] = crate::pins::attach(
            root,
            "runq",
            object,
            &[
                ("wakeup", "sched_wakeup"),
                ("wakeup_new", "sched_wakeup_new"),
                ("switch_to", "sched_switch"),
            ],
            &["runq_hist"],
        )?
        .try_into()
        .ok()
        .context("one map")?;
        Ok(Runq {
            hist: PerCpuArray::try_from(hist)?,
        })
    }
    /// The cumulative histogram of each CPU: `[cpu][bucket]`, bucket i counting waits of 2^i to 2^(i+1) µs.
    ///
    /// # Errors
    /// The map cannot be read.
    pub fn per_cpu(&self) -> Result<Vec<Vec<u64>>> {
        let mut cpus: Vec<Vec<u64>> = Vec::new();
        for (bucket, values) in self.hist.iter().enumerate() {
            for (cpu, count) in values?.iter().enumerate() {
                if cpus.len() <= cpu {
                    cpus.resize(cpu + 1, Vec::new());
                }
                cpus[cpu].resize(bucket + 1, 0);
                cpus[cpu][bucket] = *count;
            }
        }
        Ok(cpus)
    }
}
