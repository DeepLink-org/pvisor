# Local OpenSandbox startup

This engineering experiment answers B-OPENSANDBOX-ENG: with images and servers
already prepared, how long does the official SDK wait for a fresh usable
sandbox? It does not establish full protocol support or equivalent isolation.

Use OpenSandbox server/SDK/execd 1.1.0, the same pinned linux/amd64 Ubuntu base,
one CPU and 512 MiB limits. Native admission caps its entire supervisor/VMM
tree; Docker caps its container. Record this resource-accounting difference.
Native runs with its compressed pool and real execd; Docker runs the pinned
upstream server and its normal bootstrap. Neither requests network policy.
Keep image preparation, pulls, service startup and the first request outside
steady-state samples and report them separately. No warm sandbox is reused.

Start both services on distinct loopback ports and use isolated state. The
runner creates/deletes only its own sandbox IDs. A fixed seed randomizes the
order of balanced pairs. Keep every slow/failing sample; there is no exclusion
rule. Validate an environment marker, write/read a private file, and confirm
deletion. Time SDK create and first checked command separately. Keep samples,
failures, source/binary digests, upstream commit, configs and kernel/tool versions
under `benchmark/.data/`; do not publish raw engineering results in user pages.

```sh
OPEN_SANDBOX_API_KEY=... python benchmark/opensandbox/run.py \
  --native-domain 127.0.0.1:8080 --native-image ubuntu \
  --docker-domain 127.0.0.1:8081 --docker-image PINNED_UBUNTU \
  --pairs 20 --output benchmark/.data/opensandbox-BATCH/samples.json
```

Omit both Docker options for a native-only diagnostic. Such a run cannot support
a speedup claim. Use the same API key on both isolated test servers. The runner
uses server proxy endpoints for both. Report medians and bootstrap 95% intervals
for differences only after checking for multimodal distributions; do not report
P99 with this sample size.

`first_requests.py` measures a separate scenario: the first create after every
API server restart, with 20 independent server processes per arm. Configuration
JSON contains `native` and `docker`, each with an explicit trusted `argv` list,
`domain` and `image`. Supply API keys through the environment, including upstream's
`OPENSANDBOX_SERVER_API_KEY`, and use isolated empty state and a real delegated
cgroup root in the native argv. Both APIs must be ready before either create is
timed. Images, filesystem caches and Docker itself remain prepared; this is
not a cold-disk or Docker-daemon restart test. The runner owns its two service
processes, stops them after confirmed sandbox deletion, and saves each server log.

```sh
python benchmark/opensandbox/first_requests.py --configuration CONFIG.json \
  --repetitions 20 --output benchmark/.data/opensandbox-FIRST
```

## Native guest engine A/B

`engine_ab.py` serves B-SANDBOX-ENGINE-ENG and compares frozen old/upstream-service
and new/native-engine daemon artifacts on the same pinned Ubuntu base and shared
compressed pool. It measures OpenSandbox 1.1.0 and E2B 2.59.0 separately, using
3 warmup pairs and 20 seeded randomized formal pairs. Every API restart has one
first create and one subsequent steady create; all VMs are fresh. It preserves
all failures and confirms output, private files, real cgroup caps and deletion.
The first-request clock excludes API startup and image preparation. OpenSandbox
transport construction precedes its SDK create clock; E2B times its full create
call. Different SDKs are not compared against one another.

Run the driver inside an isolated delegated cgroup scope with `--output`,
`--before`, `--after`, `--images-dir`, `--opensandbox-python` and `--e2b-python`.
It uses only its own API/VM state, freezes inputs and execution order before
sampling, and stores raw data plus paired bootstrap 95% intervals in the output.
This is an internal product-path A/B, not a Docker comparison, a cold-pull test,
a concurrency benchmark or proof of complete SDK conformance.

## Native guest versus the official Docker server

B-OPENSANDBOX-ENG also permits the default native guest profile against the
pinned official server/execd 1.1.0 Docker backend. Select a `native_guest: true`
template with `--native-image` or the first-request configuration's `image`.
Keep the same base digest, SDK, entrypoint, environment and resource requests.
Archive the selected template, exact daemon artifact and corresponding source,
official source/config/image identities and both frozen runners before timing.
This compares complete deployment paths with different execution services; it
does not isolate a language/runtime change or equalize kernel isolation.
Report these new paired results separately from the historical real-execd
comparison and from native-only or old/new pVisor diagnostics.

## Full service and instance memory

`memory.py` serves B-OPENSANDBOX-MEMORY-ENG. It compares the frozen native guest
daemon with its default pool-off configuration, the same daemon with the explicit
physical page pool, and the official 1.1.0 server with a private rootless Docker
daemon. Each product slice includes its API, VM/supervisor/pool or Docker,
containerd, shims, port forwarding and containers. The SDK coordinator and the
observer stay outside every product slice. This differs from the startup
experiment's existing rootful Docker service: keep the cohorts separate.

Prepare inputs before measurement, using the exact source evidence associated
with the selected daemon. The host Docker command is used only to export the
already pinned Ubuntu and trusted execd images. It never tags, deletes or creates
objects in that Docker service. Rootless stores and sockets are private. The
installed pasta driver uses explicit pesto port forwarding; no global
KSM, sysctl, firewall or cache settings are changed.

```sh
SDK_PYTHON=/path/to/opensandbox-1.1.0/venv/bin/python
"$SDK_PYTHON" benchmark/opensandbox/memory.py --prepare \
  --inputs /tmp/opensandbox-memory-inputs \
  --daemon /path/to/frozen/pvisor-daemon \
  --images-dir /path/to/prepared/images \
  --source-evidence /path/to/matching/native-source-evidence
"$SDK_PYTHON" benchmark/opensandbox/memory.py \
  --inputs /tmp/opensandbox-memory-inputs --upstream /path/to/OpenSandbox \
  --preflight --output benchmark/.data/opensandbox-memory-preflight-BATCH
"$SDK_PYTHON" benchmark/opensandbox/memory.py \
  --inputs /tmp/opensandbox-memory-inputs --upstream /path/to/OpenSandbox \
  --rounds 5 --output benchmark/.data/opensandbox-memory-BATCH
python3 benchmark/opensandbox/analyze_memory.py \
  benchmark/.data/opensandbox-memory-BATCH
```

The daemon must contain the native guest engine and the pinned image manifest
must have `native_guest: true`. The official image identities in the preparation
script are laboratory inputs; replace them only with explicitly reviewed, pinned
same-base inputs and retain their identities. `source-evidence` supplies the
matching `source.tar.gz`, `source-receipt.json` and `validation.json`; the last
must bind the exact daemon SHA-256. A user systemd manager, delegated cgroup v2,
KVM, rootless Docker/pasta/pesto and the musl Rust target are required.

Every fresh product group has four CPU quota units, affinity 0–3, 4 GiB maximum
memory and zero swap; each sandbox requests one CPU and 512 MiB. Empty service,
one idle sandbox and four idle sandboxes are measured separately. Four copies of
the identical static worker then each retain 64 MiB of matching deterministic
data for 30 seconds. Full readback, complete instance-unique writes, a ten-second
observation and another full check exercise sharing and its degradation. The
worker blocks on guest-local sockets between commands; it does not heartbeat or
periodically touch its allocation.

Complete-subtree cgroup charged memory is the primary metric. PSS sums every
readable product process's `smaps_rollup`, proportionally charging mapped shared
pages; if any product process is non-dumpable, the complete PSS is explicitly
unavailable, never a sum that silently omits that helper. Readable PSS is retained
only as a lower bound. RSS is never added as the physical-memory headline.
Cgroup anon/file/kernel components, peak, CPU, swap and OOM events are separate
observations every 500 ms. Each phase uses the median of its
last two seconds, then formal results use the five fresh groups' median and
range with paired bootstrap intervals. Startup peaks, readback costs and the
fixed service baseline remain separate. Prepared image caches may be charged
outside the new group; neither PSS nor cgroup accounting alone is whole-host
net physical memory. These small fixed groups do not measure production density.

Keep the entire preliminary and formal directories, including failed setup or
preflight groups. Do not merge preliminary data into formal results. The
engineering synthesis belongs in the bilingual
`docs/src/{zh,en}/lab-notes/opensandbox-performance-analysis.md`, with startup,
disk inventory and memory explicitly labeled as separate cohorts.
