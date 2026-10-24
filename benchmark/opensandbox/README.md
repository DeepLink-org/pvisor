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
