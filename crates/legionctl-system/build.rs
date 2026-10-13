//! Compile the eBPF program (`bpf/runq.bpf.c`) with clang; the daemon embeds the object.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let source = "bpf/runq.bpf.c";
    println!("cargo::rerun-if-changed={source}");
    let status = Command::new("clang")
        .args([
            "-target", "bpf", "-O2", "-g", "-Wall", "-Werror", "-c", source, "-o",
        ])
        .arg(out.join("runq.bpf.o"))
        .status()
        .expect("clang is needed to build the eBPF programs");
    assert!(status.success(), "clang failed on {source}");
}
