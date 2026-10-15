// I/O latency per block device (note 04): the time from a request's issue to the driver to its completion, as a log2
// histogram in microseconds per disk (major << 20 | first minor). CO-RE: the fields read (request.q,
// request_queue.disk, gendisk.major and first_minor) are relocated against the running kernel's BTF. Built with
// `clang -target bpf -O2 -g`.

typedef unsigned int u32;
typedef int s32;
typedef unsigned long long u64;

#define SEC(name) __attribute__((section(name), used))
#define __uint(name, val) int(*name)[val]
#define __type(name, val) typeof(val) *name

// uapi constants (stable ABI)
#define BPF_MAP_TYPE_HASH 1
#define BPF_MAP_TYPE_LRU_HASH 9
#define BPF_NOEXIST 1
#define BUCKETS 32

static void *(*bpf_map_lookup_elem)(void *map, const void *key) = (void *)1;
static long (*bpf_map_update_elem)(void *map, const void *key, const void *value, u64 flags) = (void *)2;
static long (*bpf_map_delete_elem)(void *map, const void *key) = (void *)3;
static u64 (*bpf_ktime_get_ns)(void) = (void *)5;

struct gendisk {
    s32 major;
    s32 first_minor;
} __attribute__((preserve_access_index));

struct request_queue {
    struct gendisk *disk;
} __attribute__((preserve_access_index));

struct request {
    struct request_queue *q;
} __attribute__((preserve_access_index));

// request -> its issue time; bounded: a request that never completes ages out
struct {
    __uint(type, BPF_MAP_TYPE_LRU_HASH);
    __uint(max_entries, 16384);
    __type(key, u64);
    __type(value, u64);
} issued SEC(".maps");

struct hist {
    u64 buckets[BUCKETS];
};

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 256);
    __type(key, u32);
    __type(value, struct hist);
} io_hist SEC(".maps");

// block_rq_issue(struct request *rq)
SEC("tp_btf/block_rq_issue")
int issue(u64 *ctx) {
    u64 rq = ctx[0];
    u64 now = bpf_ktime_get_ns();
    bpf_map_update_elem(&issued, &rq, &now, 0);
    return 0;
}

// block_rq_complete(struct request *rq, blk_status_t error, unsigned int nr_bytes)
SEC("tp_btf/block_rq_complete")
int complete(u64 *ctx) {
    u64 rq = ctx[0];
    u64 *since = bpf_map_lookup_elem(&issued, &rq);
    if (!since) {
        return 0;
    }
    u64 us = (bpf_ktime_get_ns() - *since) / 1000;
    bpf_map_delete_elem(&issued, &rq);
    struct gendisk *disk = ((struct request *)rq)->q->disk;
    if (!disk) {
        return 0;
    }
    u32 dev = ((u32)disk->major << 20) | (u32)disk->first_minor;
    struct hist *h = bpf_map_lookup_elem(&io_hist, &dev);
    if (!h) {
        struct hist zero = {};
        bpf_map_update_elem(&io_hist, &dev, &zero, BPF_NOEXIST);
        h = bpf_map_lookup_elem(&io_hist, &dev);
        if (!h) {
            return 0;
        }
    }
    u32 bucket = 0;
    for (u32 i = 1; i < BUCKETS; i++) {
        if (us >> i) {
            bucket = i;
        }
    }
    __sync_fetch_and_add(&h->buckets[bucket & (BUCKETS - 1)], 1);
    return 0;
}

// tracing programs need a GPL-compatible license
char LICENSE[] SEC("license") = "Dual MIT/GPL";
