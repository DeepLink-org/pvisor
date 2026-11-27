# Bounded split-ring handoff on a mutable Rust native root

Engineering experiment for [B-VM-GUEST-ENG](../REGISTRY.md#b-vm-guest-eng).
The question is whether a worker and vCPU can exchange the **same** scalar
FUSE requests with fewer exits. The baseline is the #212 protocol: inline
metadata on one vCPU, ordinary synchronous writes and IRQ read-to-ack.
There is no change to the probe, validation, cache policy, or gate.

## Protocol and failure boundaries

- A host opt-in, `/dev/root`, writable plain passthrough, no DAX or managed
  or overlay backend, and `available_parallelism() > 1` are necessary to offer
  bit 21. At device creation at least two CPUs in the calling thread's affinity
  mask must have different Linux `thread_siblings_list` groups. If this cannot
  be established the device retains ordinary notifications. The guest selects
  it only on one vCPU and modern x86 `pVis` MMIO alongside bit 22. The host masks unoffered bits and enables
  ordinary queues only on activation with both bits and `VERSION_1` acknowledged
  and `EVENT_IDX` absent. Device reset clears negotiation. Bit 21 grants no
  authority to access files. Only an optimization hint crosses the boundary:
  ordinary descriptor validation, credentials, dispatch and reply parsing
  still apply. A specially marked inline kick in this mode can only wake the
  worker.
- On an otherwise empty request queue, the submitter sets `NO_INTERRUPT`
  **before** publishing `LOOKUP`, `GETATTR` or `WRITE`. It kicks if required,
  polls at most 512 iterations under the IRQ-disabled queue lock, clears
  `NO_INTERRUPT`, fences and checks used.idx again. The request owns its DMA
  buffers through completion; reply processing and any blocking wait are
  outside the queue lock. A callback
  still runs via the queue's Taskless softirq, never under this lock.
- After doing the actual backend operation outside MMIO and ring locks, the
  worker publishes payload, used element, release-store of used.idx, full
  fence, then reads the IRQ flag. If the guest's final check misses the
  completion, the host must see the arm and raise the normal IRQ. If the host
  observed suppression, the guest's post-arm check must see the completion.
  Spurious interrupts are harmless. The guest wait queue retains the
  completion status even if the IRQ precedes the call to wait.
- In the reverse direction the worker leaves `NO_NOTIFY` set while draining.
  After an eligible completion it checks avail.idx up to 512 times under the
  ring mutex and a RAM access lease. A hit returns to normal admission; a
  miss goes through the ordinary empty-ring **arm, full fence, recheck**
  before epoll. The guest fences between publishing avail.idx and testing
  `NO_NOTIFY`. Thus either the guest kicks or the worker finds the request.
  No new timer or polling thread is created. An iteration budget bounds CPU
  work, not wall time if a host thread is descheduled.
- Hiprio, busy guest queues and `FSYNC` use the ordinary completion path.
  A slow WRITE that exhausts the guest budget still waits for the host IRQ;
  the used reply carries the actual byte count or errno. With only one core
  in the initial affinity mask the offer is absent, not merely ignored.
  A larger mask does not reserve cores or detect later pinning, CPU contention
  or throttling: each missed window can still burn CPU. Neither request tail
  latency nor host CPU usage is bounded to improve. Leave this opt-in.

## Reproduce

Build one native runner as in the [gate recipe](../vm/guest.md#本地复现);
the native payload must be rebuilt, not just the VMM. On idle Linux x86_64
with KVM and disk-backed ext4/XFS/btrfs, in Bash:

```bash
set -euo pipefail
runner="$PWD/target/release/examples/vm_api_case"
mkdir -p benchmark/vm/.data
batch=$(mktemp -d "$PWD/benchmark/vm/.data/handoff.XXXXXX")
cc -O2 -static -Wall -Wextra -Werror benchmark/vm/probe.c -o "$batch/probe"
sha256sum "$runner" "$batch/probe" >"$batch/inputs.sha256"
for op in disk-warm-read disk-shuffled-read disk-write-fsync disk-shuffled-overwrite-fsync; do
  for arm in 0 1; do
    cell="$batch/$op-$arm"; root="$cell/root"
    mkdir -p "$root"/{dev,ram,work}
    cp "$batch/probe" "$root/probe"
    PVISOR_GUEST_KERNEL=rust-native PVISOR_VM_FS_QUEUE_HANDOFF=$arm \
      taskset -c 2-5 timeout 30 "$runner" --rootfs "$root" --cpus 1 --memory 256 \
        --task-stdio -- /probe "$op" </dev/null >"$cell/out" 2>"$cell/err"
    grep -xE "VM_GUEST_OK $op [1-9][0-9]*" "$cell/out"
  done
done
```

One run checks functionality, not performance. Use fresh directories for
each block and randomize order for paired timing. Repeat separately with
`taskset -c 2` for the no-offer case. On separate diagnostic boots only,
`PVISOR_FS_PROFILE=1` reports `handoff_hit`, `handoff_miss` and
`handoff_irq_suppressed` as cumulative dispatch units; these include boot
and verification, and are **not** exit counts. Use the phase-bracketed vCPU
stats FD observer described in the [#212 experiment](native-guest-file-io.md)
for actual exits. Do not draw per-syscall tail latency or CPU conclusions
from `VM_GUEST_OK`, which reports elapsed time per whole workload.

### Check the CPU admission boundary

Use `lscpu -e=CPU,CORE,SOCKET` and `taskset -pc $$` to select one allowed
CPU, two allowed SMT siblings, and two allowed CPUs on different cores.
Repeat the `disk-warm-read` command above with each mask, request handoff,
and set `PVISOR_FS_PROFILE=1`. Check the **final** `virtio-fs-dispatch`
record, not just the exit status. Only the last mask should produce handoff
counts on an eligible machine. Containers without readable CPU topology may
decline all three. This is not a physical-CPU reservation in nested VMs.

On a Ryzen 7 9700X, Linux x86_64/KVM, btrfs, the same Rust guest image
(`2320383782b1`) and probe (`a26febf3addc`) ran on the PR baseline
`0249c8ec` and the core-admission VMM. All six fresh VMs passed the byte
oracle. One profiled VM per cell, whole-run counts (including setup):

| Host affinity | Handoff hits before → after | Interpretation |
|---|---:|---|
| `6` | 0 → 0 | Neither version offers handoff. |
| `6,14` | 448 → 0 | SMT-only mask now keeps inline metadata and ordinary IRQs. |
| `5,6` | 614 → 630 | Two distinct reported cores still exercise handoff. |

These counts identify paths, not their relative speeds. A separate 15-pair
SMT-only timing run overlapped other experiments. For example, three baseline
warm-read samples took over 60 ms, the other twelve at most 21 ms; no samples
were removed. Do not use this batch to predict a speedup. Raw records and
runner hashes: `benchmark/vm/.data/physical-core-admission-0249c8ec/`.

## KVM preflight

The isolated runner (`086ad09a9f06`, native image `aa204cb68357`, static probe
`a26febf3addc`) completed the four unmodified workloads on a writable btrfs
root with the opt-in requested. On CPUs 2–5, the following are **whole-VM
cumulative counts**, including startup, setup, and byte verification:

| Original workload | Worker found next avail | Window expired | IRQ suppressed |
| --- | ---: | ---: | ---: |
| `disk-warm-read` | 2873 | 325 | 2579 |
| `disk-shuffled-read` | 2798 | 400 | 2466 |
| `disk-write-fsync` | 1999 | 176 | 1918 |
| `disk-shuffled-overwrite-fsync` | 3890 | 332 | 3682 |

All four also completed pinned to CPU 2 with **no handoff counters** and
nonzero inline-metadata counts. The same candidate passed the live
rewrite/grow/truncate/rename-recreate test, the full/short/EFBIG host-write
fixture, and the UID 1000, no-effective-capabilities publication fixture
(including set-ID behavior and host byte verification). These were functional
runs with profiling; some overlapped local compilation. They do not measure
paired speedup, CPU cost or exit deltas. The local logs are under
`target/handoff-checks/v2-xB0K/`. The runner also contains the inherited
memory-gate change; use this same executable for both transport arms.

An earlier candidate merely scheduled the async WRITE callback's Taskless
from task context. That raises a pending bit; there need not be another
device IRQ to dispatch it. A profiled shuffled overwrite took 630 ms.
The corrected version releases a bottom-half guard **after** scheduling
the Taskless and outside the queue lock, using the kernel's normal pending
softirq dispatch (and its fairness budget). The callback context and WRITE
reply checks are unchanged. The same diagnostic then took 12 ms. This
identified a progress dependency; it is not an estimate of the transport's
performance versus #212.
