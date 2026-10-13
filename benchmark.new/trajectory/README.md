# Fixed trajectory VM smoke (B-TRAJECTORY-ENG)

This engineering entry executes real tools from one command-only fixture. It
does not call a model, import an Agent runtime, or run an official SWE-bench
evaluation. Read `benchmark/README.md#b-trajectory-eng` first.

Prepare the downloaded, digest-pinned OCI layout using
`benchmark/pvisor/prepare_replay_rootfs.py --sample SAMPLE --output NEW_PREPARED
--umoci /absolute/path/to/umoci`. All prepared trees and results should live on
ordinary disk; use short paths and preserve failures. The prepared root has the
original `/testbed`, an injected `/replay-input`, and empty control/output
directories. The guest worker bind-remounts only `/replay-input` read-only in
its own VM and verifies refusal before running tools. Whole-root staging keeps
all artifacts in the configured sample stage; no host lower is applied.

Build `vm_trajectory_replay` from the recorded source and select a frozen
compatible `libkrunfw.so.5` directory for the GNU example. Point both
`CARGO_TARGET_DIR` and the repository `target` path (the embedded guest build
uses the latter) at the dedicated data volume. The example dispatches internal
VM re-exec before creating Tokio.

The template is `trajectory_smoke.json`; its distinct name preserves the existing
`just benchmark smoke` shortcut for the process benchmark. Use the explicit
preparation flow below for this VM trajectory.

Materialize and execute exactly one condition:

```sh
python3 benchmark.new/trajectory/prepare.py --batch /data/SHORT/.data/preflight -- \
  --prepared /data/PREPARED/prepared.json --binary /data/BUILD/release/examples/vm_trajectory_replay \
  --firmware /data/FIRMWARE --kind preflight
/data/BUILD/semspec/debug/semspec bench --cwd /data/SHORT/.data/preflight /data/SHORT/.data/preflight/in.json
```

Follow with `--kind handshake --mode resident`, then pause and offload. A fresh
`--kind replay --barriers ''` establishes historical fidelity; a fresh resident
replay with default barriers `1,17,39` freezes the three-point reference.
Pass `--reference /data/RESIDENT/sample` to subsequent pause/offload conditions.
Each invocation requires a NEW batch. The row is one engineering sample;
resource/phase data are custom observations, not hyperfine statistics.

Negative checks use handshake with `--scenario wrong-token`, `--scenario no-go
--wait-seconds 0`, and `--scenario cancel --mode pause` / `offload`. No-go
requires a Guest timeout, wrong-token requires explicit rejection followed by
valid release, and cancellation requires terminal native cleanup. Each normal
barrier also checks no early advance and duplicate ready observation.

The outer sampler stays outside the systemd user service. Driver, VMM and Python
launch wrapper share a 4 GiB/4 CPU/zero-swap cgroup and four fixed eligible CPUs.
Shared lower-cache charge can remain outside that group. Raw service samples,
IO/CPU/OOM counters, end-state process evidence, native control events, tool
outputs, source/binary hashes, and patches remain under the batch. Raw stdout
differences require review; exit codes/source hashes/patch equality alone do not
prove equivalent test observations. No formal performance conclusion is made
from smoke runs or runs concurrent with other host work.

Validation: `just test-py benchmark/pvisor/test_trajectory_replay.py` and build
the Rust example; actual KVM probes are required separately.

The rootless unpacker restores regular-file/directory virtual UID/GID/mode from
the verified OCI metadata using pVisor's `user.containers.override_stat` xattr.
It preserves umoci's file bytes, links, whiteouts, and physical modes. Linux
does not permit these user xattrs on symlinks; that ownership limitation is
recorded in the preparation receipt. Never hide ownership errors by adding a
Git safe-directory exception.

On a host with an existing suitable AppArmor profile, `--apparmor-profile NAME`
selects that profile for the driver. This does not install policy or change
global user-namespace settings. `--enomem-workaround auto|0|1` records an explicit
SDK compatibility override. Current SDK auto enables a 5 ms delay before each
KVM_RUN on Linux major versions below 7. The gpu02 engineering campaign uses
`0` on its 6.14.0-27 kernel; this is an experiment configuration, not a change to
the runtime's conservative default. Do not silently inherit unrelated host
PVISOR settings. Periodic faulthandler stack traversal is excluded from the
guest preflight; fatal-signal tracing and kernel logs remain available.

All 43 historical observations are checked, retaining only the frozen test
seed/timing normalization and two exact fixture differences: `/bin/bash` versus
`bash` in the step-5 missing-pytest error, and the exact step-6 first-import
warning appearing in the preflight instead. Different exception text or test
results fail. Nonzero historical tool exits are retained, never repaired by
installing pytest or editing the original commands.

After samples finish, `trajectory_report.py --data DATA --runs BASELINE RESIDENT
PAUSE OFFLOAD --output NEW_ANALYSIS` independently checks the original tool
output hashes, command bytes, all observations, worker continuity, final files,
patch, and unchanged prepared lower inventories. It writes a separate JSON
audit and phase CSV without overwriting sample evidence. Phase memory values
are medians of external 100 ms observations during the explicit wait window;
they are not instantaneous control-point measurements. Resume ACK and guest
acknowledgement are reported separately. If the user cgroup does not delegate
the IO controller, retain `io=null` with its reason instead of fabricating zero.
