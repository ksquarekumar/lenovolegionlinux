// Workload (note 04, the configsets' `workload` condition): each program start (the process id and the new name) and
// each process end, in order in one ring buffer. The daemon classifies the names; this program only reports. No kernel
// structure is read, so it needs no relocations. Built with `clang -target bpf -O2 -g`.

typedef unsigned int u32;
typedef unsigned long long u64;

#define SEC(name) __attribute__((section(name), used))
#define __uint(name, val) int(*name)[val]

// uapi constants (stable ABI)
#define BPF_MAP_TYPE_RINGBUF 27
#define COMM 16

static u64 (*bpf_get_current_pid_tgid)(void) = (void *)14;
static long (*bpf_get_current_comm)(void *buf, u32 size) = (void *)16;
static void *(*bpf_ringbuf_reserve)(void *ringbuf, u64 size, u64 flags) = (void *)131;
static void (*bpf_ringbuf_submit)(void *data, u64 flags) = (void *)132;

struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, 256 * 1024);
} events SEC(".maps");

struct event {
    u32 pid;
    // 0: the process started a program (comm is its name), 1: the process ended
    u32 ended;
    char comm[COMM];
};

static void report(u32 pid, u32 ended) {
    struct event *e = bpf_ringbuf_reserve(&events, sizeof(struct event), 0);
    if (!e) {
        return;
    }
    e->pid = pid;
    e->ended = ended;
    bpf_get_current_comm(e->comm, COMM);
    bpf_ringbuf_submit(e, 0);
}

// sched_process_exec: the new program is in place, so the name is the new one
SEC("tp_btf/sched_process_exec")
int exec(u64 *ctx) {
    report(bpf_get_current_pid_tgid() >> 32, 0);
    return 0;
}

// sched_process_exit runs for each thread; the process ends with its main thread (thread id = process id)
SEC("tp_btf/sched_process_exit")
int ended(u64 *ctx) {
    u64 id = bpf_get_current_pid_tgid();
    u32 pid = id >> 32;
    if ((u32)id == pid) {
        report(pid, 1);
    }
    return 0;
}

// tracing programs need a GPL-compatible license
char LICENSE[] SEC("license") = "Dual MIT/GPL";
