# Docker CLI contracts

The active Bats suites in `system/` and Go tests in `e2e/` exercise the pVisor
container CLI as a black box. Each test owns a private store and checks public
commands, exact output, exit status and lifecycle effects.

`upstream/` contains pinned Podman reference sources. They are excluded from
execution. [UPSTREAM.md](UPSTREAM.md) records attribution, revision and local
adaptations; `upstream-sha256.json` protects the imported inventory.

## Run

Prepare Linux/KVM or Apple Silicon/HVF, matching firmware, Python, Bats and Go.
Build the product with `mise run build`, then run from the repository root:

```sh
PVISOR_BINARY="$PWD/target/debug/pvisor" bats tests/podman/system
PVISOR_BINARY="$PWD/target/debug/pvisor" go test -v tests/podman/e2e/*.go
```

`PVISOR_BINARY` is one executable path. `PVISOR_TEST_IMAGE` selects an Alpine
fixture (default `docker.io/library/alpine:3.20`); prefer a native manifest
digest. The fixture needs `sh`, `printenv`, `id`, `cat`, `sleep`, `true`,
BusyBox netcat, `nobody`/`nogroup` and `/etc/alpine-release`. Include all Go
test files so shared helpers compile.

On macOS, sign a private binary copy and provide its adjacent firmware:

```sh
runtime_dir=$(mktemp -d)
cp target/debug/pvisor "$runtime_dir/pvisor"
ln -s "$PWD/target/debug/libkrunfw.5.dylib" "$runtime_dir/libkrunfw.5.dylib"
codesign --force --sign - --entitlements crates/pvisor/macos-hypervisor.entitlements "$runtime_dir/pvisor"
export PVISOR_BINARY="$runtime_dir/pvisor"
```

Remove that owned directory after the suite finishes. Missing virtualization,
image failures and active contract failures fail the run. No Docker daemon or
comparison executable is required.

## Isolation and evidence

Tests use private `PVISOR_DOCKER_HOME` stores, unique names and bounded commands.
Cleanup targets only owned containers and stores. Never remove a store while it
still owns a live workload.

Set `PVISOR_TEST_FAILURES` to retain failed Go stores at their existing paths.
The output contains pointers, inputs and errors; inspect cleanup failures before
manual removal. Passing stores receive normal cleanup.

Required Linux/KVM CI shards run Bats and two Go selections against the same
frozen static product artifact. Go discovers active tests and assigns each name
to one shard. Empty selections fail; shards run independently with bounded
timeouts. CI retains TAP/Go JSON, selections and host diagnostics. See
[the workflow](../../.github/workflows/ci.yml).

## Coverage and limits

The active suites cover run/exec I/O, environment and identity, lifecycle,
logs, volumes, TCP publication, image operations and local builds/cache.
They port selected contracts, not the complete upstream corpus.

Cases outside this profile are recorded with reasons in
`system/900-unsupported.bats.disabled` and `e2e/unsupported_test.go.disabled`.
Those files produce no pass or skip results. Other unported upstream cases
remain unvalidated. Public command support is defined by the
[CLI reference](../../docs/src/en/reference/cli.md).

Important limits:

- Close-to-open shared-file checks do not establish cross-VM mmap or lock
  coherence for already-open mappings.
- Named-volume persistence and fresh-container isolation do not prove physical
  deletion of anonymous backing files.
- Registry faults, authentication, TLS and mirror fallback need controlled
  service fixtures.
- Real terminal signal forwarding and VM resource limits need native runtime
  execution; parser tests cannot establish them.
- Host cgroup quotas and guest vCPU/RAM shape are different contracts.

Keep assertions on the supported behavior when adapting a source case. Preserve
upstream files and license notices; update the adaptation map for new ports.

## Chaos and soak

| Driver | Scope and instructions |
| --- | --- |
| `chaos.py` | Bounded macOS container transitions; [manual](CHAOS.md) |
| `application_chaos.py` | Linux/KVM HTTP application, VM/owner faults and shared data; [manual](APPLICATION_CHAOS.md) |
| `cli_chaos.py` | CLI and memory-pool transitions under systemd resource limits |
| `soak.py` | Linux image/compiler workloads, application chaos and Go contracts |

For CLI chaos, freeze the CLI, daemon and matching firmware. Run the whole
process tree in a test-owned systemd user service:

```sh
systemd-run --user --unit=pvisor-cli-chaos-UNIQUE \
  -p MemoryMax=1536M -p MemorySwapMax=0 -p CPUQuota=150% -p TasksMax=384 \
  -p Nice=10 -p IOSchedulingClass=idle --working-directory="$PWD" \
  python3 tests/podman/cli_chaos.py --binary /absolute/frozen/pvisor \
  --output /absolute/new/short/evidence --seconds 7200 --seed 10251526
python3 tests/podman/audit_cli_chaos.py /absolute/new/short/evidence
```

Keep the private socket/evidence path short. The driver owns at most two
1-vCPU/256-MiB VMs, verifies exact generation markers and bytes, and pauses
admission under host memory/disk/load pressure. `--scenario` selects a focused
campaign; `--inject-pool-loss` enables an explicit final owned-pool fault.
Neither an incomplete selection nor a stopped campaign is a full-profile pass.

For soak, build the static HTTP fixture from the application manual, prepare a
test-owned parent slice, and keep the controller/evidence monitor bounded:

```sh
systemctl --user start pvisor-soak-owned.slice
systemctl --user set-property --runtime pvisor-soak-owned.slice \
  MemoryMax=6G MemorySwapMax=0 CPUQuota=600% TasksMax=2048
systemd-run --user --scope --slice=pvisor-soak-owned.slice \
  -p MemoryMax=4G -p MemorySwapMax=0 -p TasksMax=1536 \
  /usr/bin/python3 tests/podman/soak.py --binary /absolute/frozen/pvisor \
  --http-binary /absolute/app-http --output /absolute/new/evidence \
  --seconds 21600 --product-slice pvisor-soak-owned.slice
```

The Linux drivers require pidfd support and delegated cgroup v2. Stop only the
owned units. Retain command bytes, input hashes, resource samples and failed
stores; cleanup errors remain failures. Audits recheck saved evidence without
starting VMs. See [test architecture](../README.md) for ordinary regressions.
