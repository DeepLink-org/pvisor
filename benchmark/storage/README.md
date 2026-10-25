# Storage inventories

## Lazy-image user decisions

`lazy_benefits.py` (B-LAZY-STARTUP) measures the current format against native
Docker with independent `storage`, `runtime` and `remote` cohorts. It reuses the
native inventory/resource adapters and semspec's clocks, repeat, command evidence
and persistence. Storage uses four pinned real images and their genuinely shared
inventory, three fresh pairs each. Runtime uses prepared Ubuntu, 3 warmups/30 pairs
and five fresh 0/1/4-instance groups. Remote independently runs Ubuntu shell,
Python standard-library checks and a complete read-only mmap scan of the Python
3.12 library tree. The scan's file count, total bytes and aggregate content hash
are derived from the verified OCI layers, including whiteouts, before execution.

Freeze a current release product and run cohorts sequentially on an otherwise
idle host. Use new output names and the input schema below:

```sh
mise run build -- release product
python3 benchmark/storage/lazy_benefits.py --phase storage \
  --binary target/x86_64-unknown-linux-musl/release/pvisor \
  --inputs /path/to/verified-inputs.json --output benchmark/storage/.data/NEW_STORAGE
python3 benchmark/storage/lazy_benefits.py --phase runtime \
  --binary target/x86_64-unknown-linux-musl/release/pvisor \
  --inputs /path/to/verified-inputs.json --output benchmark/storage/.data/NEW_RUNTIME
python3 benchmark/storage/lazy_benefits.py --phase remote \
  --binary target/x86_64-unknown-linux-musl/release/pvisor \
  --daemon target/x86_64-unknown-linux-musl/release/pvisor-daemon \
  --inputs /path/to/verified-inputs.json --output benchmark/storage/.data/NEW_REMOTE
python3 benchmark/storage/local_reads.py \
  --binary target/x86_64-unknown-linux-musl/release/pvisor \
  --inputs /path/to/verified-inputs.json --output benchmark/storage/.data/NEW_LOCAL_READS
```

Remote retains 3 warmups/30 randomized pairs per workload, each arm cold then
immediately warm, with new VMs/containers. Docker has a fresh private daemon and
empty store per pair. Its read-only registry binds only that daemon's network
namespace loopback; a private self-signed HTTPS endpoint is explicitly allowed
only by that daemon. No host trust configuration changes. pVisor has private
user/mount/PID namespaces, a pivoted root/private `/tmp`, a prepared cache service
and a framed counting proxy. No existing Host listener or Docker daemon is adopted.
Payload/client affinity is 0,1, services 2,3; requests are 2 CPU/512 MiB containers
versus 2-vCPU/512-MiB guests, not equal whole-product limits. The runtime cohort's
4-CPU/4-GiB parent groups and 1-CPU/256-MiB instances remain a separate scope.

Client clocks include native missing-image pull+run or lazy spawn, through checked
Ready and normal exit. They exclude private daemon startup and server publication.
Host page caches remain warm. Cold content must be positive; warm content must be
zero. Every trial checks private writes, image identity, workload output, VM Bundle,
normal exit and cleanup. Proxy Data hashes and exact successful body lengths are
checked; registry transfer errors fail the cohort. Counts describe response bodies
and pVisor framing, excluding HTTP headers, TLS/TCP/IP. Registry TLS and token-based
plain cache TCP have different overheads, included in full timings; this is not a
pure-format or WAN comparison. Raw remote file blocks may be larger than OCI gzip
layers; mmap uses guest page cache rather than DAX.

`local_reads.py` independently measures the prepared Python image with the same
product-group budgets as runtime and 2 CPU/512 MiB instances. Each mmap task scans
and authenticates the complete Python library tree twice in one instance, emitting
internal monotonic durations separately from host Ready/Exit. First and repeated
passes both include enumeration, mmap and SHA-256; the repeated pass measures guest
or container page-cache reuse, not decompression throughput. It retains host page
caches and does not establish random mmap, cold-disk or writable-copy-up performance.

Server publication starts from verified compressed OCI blobs, still resolves the
manifest upstream and retains staging separately. `preparation.json`, full service
and staging inventories disclose this cost; no unmatched import-speed ratio is
reported. Frozen artifacts/sources, per-trial timings/requests/Bundles, failed
preflights and process snapshots stay in `.data`. Any failed gate invalidates a
cohort; no valid slow trials are removed. Sorted gaps exceeding `max(5 ms, 10% of
the condition median)` split groups only when each has at least three samples;
publish those groups' counts/medians and every point. N30 does not establish P99.

## Native Docker engineering adapters

`native_docker.py --phase storage` (B-IMAGE-STORAGE-ENG, `image_storage.json`)
compares current V2 stores with a fresh native rootless Docker/containerd overlayfs
store. It includes both Docker compressed blobs and unpacked snapshots, preserves
layer sharing, records an empty-daemon baseline and measures real combined stores.
It never uses an existing daemon or a manually extracted tree as the Docker result.

`--phase runtime` (B-NATIVE-DOCKER-ENG, `native_runtime.json`) independently measures
prepared Ubuntu CLI Ready/Exit and 0/1/4-instance cgroup memory. It uses semspec's
Ready clock, repeat and command-evidence machinery. Both products enter fresh private
4-CPU/4-GiB/swap0 slices through the same small frozen launcher; an identical pinned
sleep process retains a delegated leaf and is included in both empty baselines.
The same image's taskset pins the payload before execution (host CPU IDs for Docker,
guest CPU 0 for the one-vCPU VM). This is affinity placement, not cpuset delegation.
Snapshot witnesses do not establish every transient init thread's past affinity.

The input JSON is a list with `name`, `pinned` (linux/amd64 manifest reference),
`digest`, `manifest` (parsed OCI manifest), and `source` (directory containing
`manifest.json` and config/layer files named by digest without `sha256:`).
An optional `manifest_path` identifies a separately named manifest. Every manifest,
config and layer is size/digest-checked and copied into the new batch before use.
Prepare these immutable sources before measuring; source bytes are excluded from
either product inventory. Freeze a current release product with `mise run build -- release product`.

```sh
python3 benchmark/storage/native_docker.py --phase storage \
  --binary target/x86_64-unknown-linux-musl/release/pvisor \
  --inputs /path/to/verified-inputs.json --output benchmark/storage/.data/NEW_DISK \
  --repeats 3
python3 benchmark/storage/native_docker.py --phase runtime \
  --binary target/x86_64-unknown-linux-musl/release/pvisor \
  --inputs /path/to/verified-inputs.json --output benchmark/storage/.data/NEW_RUNTIME \
  --runs 30 --memory-rounds 5
```

Both phases require Linux/KVM, Btrfs with per-directory `chattr +m`, rootless
Docker tools and retained inputs. Runtime also requires delegated user cgroups,
systemd/busctl and rustc. Only new private directories/units are created. Complete
stores and failed evidence remain in `.data`; owned services stop on exit.
Disk inventories use `st_blocks*512`, not exclusive physical-device bytes. Memory
is complete product-group accounting, not configured guest RAM, process RSS or
whole-host added physical memory. VM and container isolation/limit scopes differ.

Keep manifests, per-file inventories, logs and frozen artifacts in the private
batch's `.data/` directory. Do not publish storage results as execution latency.

`image_format.py` remains an internal B-IMAGE-FORMAT-ENG format-regression runner;
it is not the native-Docker product comparison.
It reuses semspec's command evidence and repeat machinery. Freeze two release
Rust test executables containing the same ignored measurement test, retain the
baseline runner source, then invoke it with `--baseline`, `--candidate`,
`--baseline-runner`, and a new `--output .data/NAME`. Formal batches use
`--runs 30 --warmup 3 --cpu N`; vary `--files`, `--directories` and `--backend`.
It validates content/counts before accepting results and saves complete artifacts,
plans, logs, paired timings, object inventories and bootstrap intervals. Hosts
retain their page cache; new reader processes start with empty decoded caches.
This measures storage stages, not VM startup. Filesystem compression and allocation
accounting are recorded; st_blocks is not exclusive physical device consumption.
