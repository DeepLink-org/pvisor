# Local OpenSandbox startup with native VM isolation

## Motivation

The managed pVisor backend reduces the first sandbox wait after an API server
restart. Steady creation remains slower than the official Docker backend. It is
a usable local replacement for the tested command/file/lifecycle profile, with
a separate guest kernel and compressed image pool; it is not a full protocol or
isolation-equivalence claim.

In 20 pairs per scenario, first creation took 213.42 ms versus 354.32 ms:
39.8% lower latency, with a paired-bootstrap 95% interval of 38.6–41.4%.
Steady creation took 233.36 ms versus 154.67 ms. The first-request improvement
does not establish faster creation for a continuously running API service.

## Experiment design

We measure official SDK create completion as sandbox readiness latency. This
includes the backend's actual admission/readiness checks and SDK endpoint/health
requests. Lower latency reduces the wait before an application can use its
sandbox. A separate metric measures create through completion of the first
checked command, including its stdout and streaming completion.

Both arms use OpenSandbox Python SDK/server/execd 1.1.0 and identical execd binary
bytes, with an Ubuntu linux/amd64 base pinned to manifest
`sha256:a2d43bffc46670d7108bb3c0468c67c5557c4c92e83e1e34cbff1322ad1b94dc`.
The host runs Linux `7.2.11-200.fc44.x86_64`; pVisor uses the static musl
`performance` build. Images and the Docker daemon are prepared before timing.
Neither arm requests network policy. Optional bwrap/hardening launchers are not
injected into either prepared image.

Every sandbox requests one CPU and 512 MiB. Native cgroups cover its supervisor,
VMM and helpers; Docker limits cover its container. The native profile verifies
VM controls, bound guest identity and authenticated service access. The upstream
Docker server uses its normal bootstrap and default init setting. These are
explicit deployment differences, not equal kernel isolation or equal available
guest-memory budgets. Native execd is in the compressed template; upstream Docker
fetches/caches its execd archive through its normal helper-container path.

Each scenario uses 20 seeded, randomly ordered pairs of fresh sandboxes. The
steady scenario keeps both API processes running. The first-request scenario
restarts both API processes for each pair and waits for both APIs before timing;
service process startup is excluded, filesystem caches remain warm, and no warm
sandbox is reused. This is not a cold-disk, image-download or Docker-daemon
restart experiment. The two scenarios are analyzed separately.

All samples must return the environment marker, verify a newly private file,
and confirm deletion through HTTP 404. All 82 timed creations, including the
two initial requests outside steady statistics, passed and were deleted. No
sample was discarded. Confidence intervals use 10,000 paired bootstrap resamples
with seed 20261024. Sorted samples are compact without a clear second cluster;
this sample count does not support P99 claims.

## Data and analysis

| Scenario | Metric | pVisor median ms | Docker median ms | Native − Docker, 95% interval ms |
| --- | --- | ---: | ---: | ---: |
| First after API restart | SDK create | 213.42 | 354.32 | −148.79 to −135.10 |
| First after API restart | First checked command | 439.87 | 562.58 | −131.74 to −111.94 |
| Steady | SDK create | 233.36 | 154.67 | +72.59 to +86.90 |
| Steady | First checked command | 457.01 | 364.04 | +85.53 to +101.82 |

The prepared native template avoids the upstream server's first-request execd
archive preparation. That advantage is specific to the first-request scenario.
Steady native creation has additional VM and service-control work; these results
do not isolate its individual causes. They do not measure concurrent throughput,
density, disk savings, notebook languages, network enforcement or hostile-user
security. Unsupported policy/snapshot/template/application-port features remain
explicit capability limits.

The final artifact passed 111 default Rust tests, both explicitly delegated
cgroup regressions, warnings-denied Clippy, and the unmodified SDK on real KVM.
SDK validation covers concurrent creation, both endpoint modes, streaming
stdout/stderr, nonzero exit, env/cwd, secret filtering, UTF-8 and >3 MiB binary
files, private writes, metrics, renewal, pause/resume, reconnect and scoped auth.
Separate native validation confirms daemon restart and port-zero endpoint
publication. A Unix-backed transport regression rejects direct loopback init
bypass and verifies >2 MiB streamed bodies and repeated response headers.

Raw evidence is local under
[`../.data/opensandbox-protected-2s287db4/`](../.data/opensandbox-protected-2s287db4/):
`summary.json`, both `samples.json` files, `receipt.json`, `audit.json`, frozen
source/binary/image pool/harness, upstream source, versions and complete logs.
These engineering results are not published into user benchmark pages.
