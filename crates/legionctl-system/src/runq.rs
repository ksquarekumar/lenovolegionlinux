//! The run-queue latency sensor (note 04): loads `bpf/runq.bpf.c` and reads its per-CPU log2 histogram.

use anyhow::{Context, Result};
use aya::maps::{MapData, PerCpuArray};
use aya::programs::BtfTracePoint;
use aya::programs::links::FdLink;
use aya::{Btf, Ebpf};
use legionctl_machine::sysroot::SysRoot;

use crate::pins::Pins;

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
        let pins = Pins::of(root, "runq", object);
        if let Some(hist) = pins.map("runq_hist") {
            return Ok(Runq {
                hist: PerCpuArray::try_from(hist)?,
            });
        }
        let mut bpf = Ebpf::load(object).context("loading the runq object")?;
        let btf = Btf::from_sys_fs().context("reading the kernel BTF")?;
        let mut links = Vec::new();
        for (program, tracepoint) in [
            ("wakeup", "sched_wakeup"),
            ("wakeup_new", "sched_wakeup_new"),
            ("switch_to", "sched_switch"),
        ] {
            let p: &mut BtfTracePoint = bpf
                .program_mut(program)
                .with_context(|| format!("no program {program}"))?
                .try_into()?;
            p.load(tracepoint, &btf)
                .with_context(|| format!("loading {program}"))?;
            let link = p
                .attach()
                .with_context(|| format!("attaching {program} to {tracepoint}"))?;
            links.push((program, FdLink::from(p.take_link(link)?)));
        }
        let hist = bpf.take_map("runq_hist").context("no runq_hist map")?;
        pins.pin(links, &[("runq_hist", &hist)])?;
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
