pub const GROUP: &str = "vm_guest";
pub const REFERENCE: &str = "firecracker";
pub const BACKENDS: [&str; 3] = ["linux", "rust-native", REFERENCE];
pub const METRICS: [&str; 15] = [
    "cpu-integer",
    "syscall-getpid",
    "ipc-pipe-roundtrip",
    "memory-copy",
    "memory-map-touch",
    "disk-write-fsync",
    "disk-warm-read",
    "disk-shuffled-read",
    "disk-shuffled-overwrite-fsync",
    "tmpfs-write",
    "tmpfs-read",
    "tmpfs-shuffled-read",
    "tmpfs-shuffled-overwrite",
    "tmpfs-metadata",
    "tmpfs-dirscan",
];

pub fn cells() -> impl Iterator<Item = (&'static str, &'static str, String)> {
    BACKENDS.into_iter().flat_map(|backend| {
        METRICS
            .into_iter()
            .map(move |metric| (backend, metric, format!("{backend}-{metric}")))
    })
}
