// Thermal trips (note 04): each time a thermal zone crosses one of its trip points (thermal:thermal_zone_trip), a
// count per zone id. CO-RE: the one field read (thermal_zone_device.id) is relocated against the running kernel's
// BTF. Built with `clang -target bpf -O2 -g`.

typedef unsigned int u32;
typedef int s32;
typedef unsigned long long u64;

#define SEC(name) __attribute__((section(name), used))
#define __uint(name, val) int(*name)[val]
#define __type(name, val) typeof(val) *name

// uapi constants (stable ABI)
#define BPF_MAP_TYPE_HASH 1
#define BPF_NOEXIST 1

static void *(*bpf_map_lookup_elem)(void *map, const void *key) = (void *)1;
static long (*bpf_map_update_elem)(void *map, const void *key, const void *value, u64 flags) = (void *)2;

struct thermal_zone_device {
    s32 id;
} __attribute__((preserve_access_index));

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 256);
    __type(key, u32);
    __type(value, u64);
} trips SEC(".maps");

// thermal_zone_trip(struct thermal_zone_device *tz, int trip, enum thermal_trip_type trip_type)
SEC("tp_btf/thermal_zone_trip")
int trip(u64 *ctx) {
    u32 zone = ((struct thermal_zone_device *)ctx[0])->id;
    u64 *count = bpf_map_lookup_elem(&trips, &zone);
    if (!count) {
        u64 zero = 0;
        bpf_map_update_elem(&trips, &zone, &zero, BPF_NOEXIST);
        count = bpf_map_lookup_elem(&trips, &zone);
        if (!count) {
            return 0;
        }
    }
    __sync_fetch_and_add(count, 1);
    return 0;
}

// tracing programs need a GPL-compatible license
char LICENSE[] SEC("license") = "Dual MIT/GPL";
