// Session activity (note 04): each program that a process in a watched cgroup (an agent session's systemd scope)
// starts, with its pid and the start of its argument list, into a ring buffer. CO-RE: the fields read
// (task_struct.mm, mm_struct.arg_start and arg_end) are relocated against the running kernel's BTF. Built with
// `clang -target bpf -O2 -g`.

typedef unsigned int u32;
typedef unsigned long long u64;

#define SEC(name) __attribute__((section(name), used))
#define __uint(name, val) int(*name)[val]
#define __type(name, val) typeof(val) *name

// uapi constants (stable ABI)
#define BPF_MAP_TYPE_HASH 1
#define BPF_MAP_TYPE_RINGBUF 27
#define ARGS 512

static void *(*bpf_map_lookup_elem)(void *map, const void *key) = (void *)1;
static u64 (*bpf_get_current_pid_tgid)(void) = (void *)14;
static u64 (*bpf_get_current_cgroup_id)(void) = (void *)80;
static long (*bpf_probe_read_user)(void *dst, u32 size, const void *src) = (void *)112;
static void *(*bpf_ringbuf_reserve)(void *ringbuf, u64 size, u64 flags) = (void *)131;
static void (*bpf_ringbuf_submit)(void *data, u64 flags) = (void *)132;

struct mm_struct {
    unsigned long arg_start;
    unsigned long arg_end;
} __attribute__((preserve_access_index));

struct task_struct {
    struct mm_struct *mm;
} __attribute__((preserve_access_index));

// cgroup id -> 1, kept by legionctl-system from the sessions' scopes
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 1024);
    __type(key, u64);
    __type(value, u32);
} watched SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, 256 * 1024);
} execs SEC(".maps");

struct exec {
    u64 cgroup;
    u32 pid;
    u32 len;
    // NUL-separated arguments, cut at ARGS bytes
    char args[ARGS];
};

// sched_process_exec(struct task_struct *p, pid_t old_pid, struct linux_binprm *bprm): the new image is in place
SEC("tp_btf/sched_process_exec")
int exec(u64 *ctx) {
    u64 cgroup = bpf_get_current_cgroup_id();
    if (!bpf_map_lookup_elem(&watched, &cgroup)) {
        return 0;
    }
    struct exec *e = bpf_ringbuf_reserve(&execs, sizeof(struct exec), 0);
    if (!e) {
        return 0;
    }
    struct task_struct *task = (struct task_struct *)ctx[0];
    struct mm_struct *mm = task->mm;
    unsigned long start = mm->arg_start;
    unsigned long len = mm->arg_end - start;
    if (len > ARGS) {
        len = ARGS;
    }
    e->cgroup = cgroup;
    e->pid = bpf_get_current_pid_tgid() >> 32;
    // the arguments were just copied onto the new stack, so the pages are present
    e->len = bpf_probe_read_user(e->args, len, (const void *)start) == 0 ? len : 0;
    bpf_ringbuf_submit(e, 0);
    return 0;
}

// tracing programs need a GPL-compatible license
char LICENSE[] SEC("license") = "Dual MIT/GPL";
