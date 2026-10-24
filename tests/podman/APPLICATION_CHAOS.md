# Docker-style application chaos

`application_chaos.py` is an opt-in Linux/KVM black-box test of applications using
the supported Docker CLI profile. It does not need a Docker daemon and adds no
Podman contract count. It is a correctness experiment, not a performance benchmark.

The fixture is an Alpine-hosted HTTP application, a network client and a read-only
shared-volume observer. A fourth VM is allowed only for an isolated-network or
short-lived container/build check. All containers use one vCPU and 384 MiB guest
RAM inside independent systemd user scopes with 512 MiB host MemoryMax, zero swap,
one CPU quota, TasksMax=256 and inherited affinity CPU 4–7. The controller uses
CPU 8–9. Linux cgroup v2, a delegated user systemd manager, KVM, Python 3.10+ and
a native pVisor executable with its firmware are required.

Every run owns a new private store and two named networks. The base image must be
a pinned native Alpine reference. The runner pulls it, builds an application image
with COPY-only instructions, freezes provenance and a seeded schedule, then runs
every scenario once before randomized repeats. Runtime failures are never skipped
or retried as successful rounds. Explicit readiness waits have bounded deadlines.
Commands, streams, HTTP results, cgroup samples and owner diagnostics are retained
on both success and failure. Cleanup attempts every exact owned name and network;
it never deletes the store or hides a cleanup error.

```sh
rustc --edition=2024 -O --target x86_64-unknown-linux-musl -C linker=rust-lld \
  tests/podman/application_http.rs -o /absolute/path/to/app-http
python3 tests/podman/application_chaos.py \
  --binary /absolute/path/to/frozen/pvisor \
  --http-binary /absolute/path/to/app-http \
  --image mirror.gcr.io/library/alpine@sha256:c64c687cbea9300178b30c95835354e34c4e4febc4badfe27102879de0483b5e \
  --output /absolute/new/evidence-directory --seed 11 --rounds 36
```

The seed reproduces scenario selection and payloads, not operating-system
scheduling. Upper state and entrypoint counters must survive stop/start and
owner/VMM crashes; named-volume state must also survive removal/recreation.
An acknowledged write is checked after the application has closed and renamed
the file; no database transaction, host power-loss or open-mapping durability is
claimed. HTTP and CLI streams are compared byte-for-byte. Network and read-only
checks include positive controls. Expected faults are explicitly distinguished
from an unexpected command or content failure.

The existing `chaos.py` retains its independent lightweight macOS profile.
