// Run-queue latency (note 04, the jank metric): the time from a task's wakeup (or preemption) to the moment it runs,
// as a log2 histogram in microseconds, per CPU. CO-RE: the one field read (task_struct.pid) is relocated against the
// running kernel's BTF, so no kernel headers are needed. Built with `clang -target bpf -O2 -g`.

typedef unsigned int u32;
typedef int s32;
typedef unsigned long long u64;

#define SEC(name) __attribute__((section(name), used))
#define __uint(name, val) int(*name)[val]
#define __type(name, val) typeof(val) *name

// uapi constants (stable ABI)
#define BPF_MAP_TYPE_PERCPU_ARRAY 6
#define BPF_MAP_TYPE_LRU_HASH 9
#define TASK_RUNNING 0
#define BUCKETS 32

static void *(*bpf_map_lookup_elem)(void *map, const void *key) = (void *)1;
static long (*bpf_map_update_elem)(void *map, const void *key, const void *value, u64 flags) = (void *)2;
static long (*bpf_map_delete_elem)(void *map, const void *key) = (void *)3;
static u64 (*bpf_ktime_get_ns)(void) = (void *)5;

struct task_struct {
    s32 pid;
} __attribute__((preserve_access_index));

// bounded: a pid that never runs again ages out
struct {
    __uint(type, BPF_MAP_TYPE_LRU_HASH);
    __uint(max_entries, 16384);
    __type(key, u32);
    __type(value, u64);
} queued SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, BUCKETS);
    __type(key, u32);
    __type(value, u64);
} runq_hist SEC(".maps");

static int enqueue(struct task_struct *p) {
    u32 pid = p->pid;
    if (pid == 0) {
        return 0;
    }
    u64 now = bpf_ktime_get_ns();
    bpf_map_update_elem(&queued, &pid, &now, 0);
    return 0;
}

SEC("tp_btf/sched_wakeup")
int wakeup(u64 *ctx) { return enqueue((struct task_struct *)ctx[0]); }

SEC("tp_btf/sched_wakeup_new")
int wakeup_new(u64 *ctx) { return enqueue((struct task_struct *)ctx[0]); }

// sched_switch(bool preempt, struct task_struct *prev, struct task_struct *next, unsigned int prev_state)
SEC("tp_btf/sched_switch")
int switch_to(u64 *ctx) {
    struct task_struct *prev = (struct task_struct *)ctx[1];
    struct task_struct *next = (struct task_struct *)ctx[2];
    // a preempted task waits in the run queue again
    if ((u32)ctx[3] == TASK_RUNNING) {
        enqueue(prev);
    }
    u32 pid = next->pid;
    u64 *since = bpf_map_lookup_elem(&queued, &pid);
    if (!since) {
        return 0;
    }
    u64 us = (bpf_ktime_get_ns() - *since) / 1000;
    bpf_map_delete_elem(&queued, &pid);
    u32 bucket = 0;
    for (u32 i = 1; i < BUCKETS; i++) {
        if (us >> i) {
            bucket = i;
        }
    }
    u64 *count = bpf_map_lookup_elem(&runq_hist, &bucket);
    if (count) {
        *count += 1;
    }
    return 0;
}

// tracing programs need a GPL-compatible license
char LICENSE[] SEC("license") = "Dual MIT/GPL";
