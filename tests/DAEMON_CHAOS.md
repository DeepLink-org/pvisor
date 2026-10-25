# Bounded native daemon chaos

`daemon_chaos.py` drives the unmodified OpenSandbox Python SDK 1.1.0 against
four real Linux/KVM native guest VMs. It checks correctness, not throughput or
production reliability. Seeds reproduce scenario/payload choices; OS scheduling
and dynamically allocated sandbox IDs remain nondeterministic.

Prepare a digest-pinned native Ubuntu template and build/freeze the daemon and
its adjacent firmware first; see the [daemon guide](../crates/pvisor-daemon/README.md).
An existing pooled-image store and SDK Python environment are required. No image
pull, Docker daemon, global sysctl/cache change or memory-filling workload is used.

Create a **new, test-owned** systemd user slice before running. For example:

```sh
systemd-run --user --quiet --collect --unit=pvisorchaosanchor \
  --slice=pvisorchaos.slice -p RuntimeMaxSec=3600 /usr/bin/sleep 3600
systemctl --user set-property --runtime pvisorchaos.slice \
  MemoryMax=2G MemoryHigh=1536M MemorySwapMax=0 CPUQuota=400% TasksMax=2048
python3 tests/daemon_chaos.py \
  --binary /absolute/frozen/pvisor-daemon \
  --prepared-manifest /absolute/images/native-ubuntu.json \
  --pool-store /absolute/images/store \
  --sdk-python /absolute/sdk-venv/bin/python \
  --product-slice pvisorchaos.slice \
  --output /absolute/new/evidence-directory --seed 11 --rounds 2000 --seconds 900
systemctl --user stop pvisorchaos.slice
```

The driver requires a new output directory. It creates a new private manifest
pointing to the supplied pool, preserving the original input. Each VM requests
1 CPU/256 MiB, including host supervisor/VMM; admission is four CPUs/1 GiB.
The aggregate slice also contains the API and the SDK worker. The SDK worker
has a separate 256 MiB cap and one-CPU quota. Swap is disabled. Every observed
live guest must retain its exact CPU/memory/PID limits; no more than four may
exist. One observer records actual cgroup usage, PID-limit events, process
thread counts and FD counts. A failure also attempts bounded guest `/proc`
snapshots before cleanup, without launching more workload processes.

Replacement payloads are at most 65,537 bytes. Each VM retains two independent
file slots and one small heartbeat; replacement does not accumulate payload
files. An append wave temporarily adds one file of at most 65,544 bytes per VM
and eight concurrent writers, then removes it after content verification.
The caller must separately monitor the campaign's allocated disk blocks and
host available memory, stop on unsafe growth, and bound retained logs. Sparse
VM files' logical size is not allocated disk usage. Evidence can be archived
only after every file's content and metadata have been verified. Failed cohorts
retain their original bytes, metadata and failed status; archiving never turns
a failed run into acceptance evidence.

The initial schedule covers every scenario before seeded random repeats:

| Scenario | Required result |
| --- | --- |
| Concurrent files | Exact binary bytes, atomic move, size/listing, single byte ranges, chmod including `000`, guest SHA-256 and SDK readback agree |
| Concurrent commands | Literal argv/UTF-8, separate stdout/stderr and distinct nonzero exits, including 125, stay correctly routed |
| Concurrent append | Eight writers per VM append independent byte values; exact total length and each writer's byte count survive without assuming shell record atomicity |
| Background jobs | Completion/status and byte cursors reproduce the exact expected output |
| Pause/resume | Acknowledged live controls retain heartbeat and all committed files |
| API crash | Kill only the pidfd-verified owned API process; restart under a stable delegated unit and recover the same sandbox IDs and data |
| In-flight background commit | Acknowledged background job waits behind a file gate, survives API death with the same job ID, then atomically commits exact bytes and reproduces its completion/logs after the gate opens |
| Delete/recreate | Delete confirms 404, admission can be reused and the new VM has private data |
| Foreground cancellation | A started process cannot create its delayed marker after its SDK request is cancelled |

`--profile steady` keeps the same four VMs and API process for the whole run.
It selects files, commands, jobs, pause/resume, cancellation and concurrent
append, leaving API death and replacement to the default `chaos` profile.
Use a longer bounded batch, for example `--rounds 20000 --seconds 2400`, to
observe FD/thread/memory retention over a sustained owner lifetime. The initial
schedule covers every scenario selected by that profile; evidence records the
profile and verifies that a steady run creates exactly four identities.

A continuous reader checks sandbox-specific heartbeats. Only connection/timeout
errors and 404/409/502/503 overlapping a fault epoch for that reader's exact
sandbox are expected. An API crash affects all current identities; pause and
deletion affect only their target. Healthy peers retain strict checks.
A 401 is expected only when an in-flight reader targets the exact identity
being deleted or already deleted during the overlapping fault. Authentication
errors for live identities and all content mismatches fail. Foreground checks
after fault completion must pass without harness retries. The SDK retains its
own pinned retry behavior.

The stable service wrapper survives API death. Restarting the **systemd unit**
instead can reset delegated child resource attributes and is not equivalent to
crashing only the API. Cleanup attempts every observed owned sandbox, accepts
an already completed 404 deletion, stops only its own unit and retains private
state for diagnosis. The caller's parent slice is not configured or stopped by
the driver.
Before creating VMs, the driver requires an empty API inventory and rejection
of an unrelated credential. Each sandbox carries a unique campaign metadata
marker. Controller cleanup deletes only matching records; it refuses unrelated
records, and does not enumerate/delete API records before endpoint verification.

The wrapper records child process exits separately from injected pidfd kills.
An unplanned API exit fails even if the wrapper restarts quickly enough that
SDK traffic did not observe the outage; the auditor reconciles exact exit PIDs.

Evidence includes the frozen driver, private config/manifest, schedule,
per-operation seeds and content hashes, progress/failure, resource/FD samples,
worker/API logs and confirmed cleanup. Kernel/process snapshots are observations,
not proof of absence of all leaks or isolation against a hostile host UID.

After a successful run, reconcile the saved evidence independently:

```sh
python3 tests/audit_daemon_chaos.py /absolute/evidence-directory
```

This regenerates seeded payload hashes, matches file starts/completions and
scenario counts, verifies frozen inputs and checks recorded VM/FD limits. It
does not replay every HTTP response; byte/permission/stream assertions remain
in the frozen live driver. A failed run is never converted into a passing run
by this audit.

Ordinary harness safety regressions need no SDK or VM:

```sh
mise run test-py -- tests/test_daemon_chaos.py tests/test_application_chaos.py
```

## Local campaign receipt

The 2026-10-25 Linux x86_64/KVM campaign retains its local, gitignored evidence
under `tests/.data/pvisor-soak-20261025/`. `results.json` separates successful
cohorts by frozen binary and keeps failed cohorts and their diagnoses;
`fixes.json` links each product fix to its failing regression or runtime trace.
The campaign uses both profiles, an aggregate 2 GiB runtime cap, zero swap,
at most four VMs and a separate 2 GiB allocated-evidence budget. Build artifacts
are outside that evidence budget. No memory-filling or disk-filling workload
is used.

Verified archives retain file bytes, modes, ownership, links and xattrs, with
an entry inventory, archive SHA-256 and restore command in each `.archive.json`
receipt. Restore an archived cohort before rerunning its auditor. Earlier
product versions and fixture failures remain evidence of their own scope;
they are not merged into final-version acceptance or performance claims.
