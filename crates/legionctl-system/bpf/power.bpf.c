// Power attribution and throttle triggers (note 04). Each CPU's run time goes to the process that ran, weighted by
// the CPU's clock at that time (µs × MHz, an estimate of its CPU energy), in a bounded map. A drop of a CPU's clock
// (power:cpu_frequency) and a thermal interrupt (thermal_apic_entry: thermal throttling starts or stops) go into a
// ring buffer, at most one each 100 ms: legionctl-system then reads the MSR throttle status. CO-RE: the fields read
// (task_struct.tgid, group_leader, comm) are relocated against the running kernel's BTF. Built with
// `clang -target bpf -O2 -g`.

typedef unsigned int u32;
typedef int s32;
typedef unsigned long long u64;

#define SEC(name) __attribute__((section(name), used))
#define __uint(name, val) int(*name)[val]
#define __type(name, val) typeof(val) *name

// uapi constants (stable ABI)
#define BPF_MAP_TYPE_ARRAY 2
#define BPF_MAP_TYPE_LRU_HASH 9
#define BPF_MAP_TYPE_RINGBUF 27
#define CPUS 1024
#define COMM 16
#define QUIET_NS 100000000ULL

static void *(*bpf_map_lookup_elem)(void *map, const void *key) = (void *)1;
static long (*bpf_map_update_elem)(void *map, const void *key, const void *value, u64 flags) = (void *)2;
static u64 (*bpf_ktime_get_ns)(void) = (void *)5;
static u32 (*bpf_get_smp_processor_id)(void) = (void *)8;
static long (*bpf_probe_read_kernel)(void *dst, u32 size, const void *src) = (void *)113;
static void *(*bpf_ringbuf_reserve)(void *ringbuf, u64 size, u64 flags) = (void *)131;
static void (*bpf_ringbuf_submit)(void *data, u64 flags) = (void *)132;

struct task_struct {
    s32 tgid;
    struct task_struct *group_leader;
    char comm[COMM];
} __attribute__((preserve_access_index));

// per CPU: when its current task started to run, and its clock (kHz; legionctl-system writes the clock at load,
// the tracepoint each change after)
struct cpu {
    u64 since;
    u32 khz;
    u32 pad;
};

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, CPUS);
    __type(key, u32);
    __type(value, struct cpu);
} cpus SEC(".maps");

// process id -> its cumulative weight and name; bounded: a process that ended ages out
struct energy {
    u64 weight;
    char comm[COMM];
};

struct {
    __uint(type, BPF_MAP_TYPE_LRU_HASH);
    __uint(max_entries, 8192);
    __type(key, u32);
    __type(value, struct energy);
} energy SEC(".maps");

// the time of the last trigger (one entry)
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1);
    __type(key, u32);
    __type(value, u64);
} last SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, 4096);
} triggers SEC(".maps");

static void trigger(void) {
    u32 zero = 0;
    u64 *at = bpf_map_lookup_elem(&last, &zero);
    u64 now = bpf_ktime_get_ns();
    if (!at || now - *at < QUIET_NS) {
        return;
    }
    *at = now;
    u32 *cpu = bpf_ringbuf_reserve(&triggers, sizeof(u32), 0);
    if (cpu) {
        *cpu = bpf_get_smp_processor_id();
        bpf_ringbuf_submit(cpu, 0);
    }
}

// sched_switch(bool preempt, struct task_struct *prev, struct task_struct *next, unsigned int prev_state)
SEC("tp_btf/sched_switch")
int switch_to(u64 *ctx) {
    struct task_struct *prev = (struct task_struct *)ctx[1];
    u32 id = bpf_get_smp_processor_id();
    struct cpu *c = bpf_map_lookup_elem(&cpus, &id);
    if (!c) {
        return 0;
    }
    u64 now = bpf_ktime_get_ns();
    u32 tgid = prev->tgid;
    // the idle task (0) is not a process; the first switch of a CPU has no start
    if (tgid && c->since) {
        // a CPU without a known clock weighs by time alone (all such CPUs alike)
        u64 mhz = c->khz / 1000 ? c->khz / 1000 : 1;
        u64 weight = (now - c->since) / 1000 * mhz;
        struct energy *e = bpf_map_lookup_elem(&energy, &tgid);
        if (e) {
            __sync_fetch_and_add(&e->weight, weight);
            // the name after an exec
            bpf_probe_read_kernel(e->comm, COMM, prev->group_leader->comm);
        } else {
            struct energy fresh = {.weight = weight};
            bpf_probe_read_kernel(fresh.comm, COMM, prev->group_leader->comm);
            bpf_map_update_elem(&energy, &tgid, &fresh, 0);
        }
    }
    c->since = now;
    return 0;
}

// cpu_frequency(unsigned int frequency, unsigned int cpu_id): the new clock in kHz
SEC("tp_btf/cpu_frequency")
int frequency(u64 *ctx) {
    u32 khz = (u32)ctx[0];
    u32 id = (u32)ctx[1];
    struct cpu *c = bpf_map_lookup_elem(&cpus, &id);
    if (!c) {
        return 0;
    }
    u32 old = c->khz;
    c->khz = khz;
    if (khz < old) {
        trigger();
    }
    return 0;
}

// thermal_apic_entry(int vector): the thermal interrupt, also when the cpufreq driver reports no clock changes
// (intel_pstate with HWP)
SEC("tp_btf/thermal_apic_entry")
int thermal(u64 *ctx) {
    trigger();
    return 0;
}

// tracing programs need a GPL-compatible license
char LICENSE[] SEC("license") = "Dual MIT/GPL";
