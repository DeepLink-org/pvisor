# Cooperative guest allocator reclamation

PID 1 controls a preloaded SO worker over a private socketpair. Applications need
no source changes or reclaim hooks, but must load the SO when starting. Requests
select a fixed allocator operation; there is no runtime code injection.

## Build and activate

Build the SO and native test fixtures using the repository's pinned Zig compiler:

```sh
mise exec -- sh tools/allocator-reclaim/build.sh aarch64 /tmp/reclaim
# Use x86_64 instead for an x86_64 guest.
```

Outputs are libpvisor-reclaim.so, subject, interposer.so (test only) and
static-subject (test only). The SO targets the glibc 2.17 ABI and links pthread/dl
normally, supporting both split-libpthread and merged-libc layouts. This build
baseline is not proof that every glibc 2.17 distribution was executed.
No new Cargo dependency or compiler dependency is added to ordinary guest builds.

Stage the matching SO in the guest image, or an existing read-only payload/share.
Use the shared launch contract, for example:

```json
{
  "argv": ["/usr/bin/my-service"],
  "env": {},
  "cwd": "/",
  "control": true,
  "process": {
    "new_session": true,
    "allocator_reclaim": "/opt/pvisor/libpvisor-reclaim.so"
  }
}
```

This is an explicit low-level GuestConfig option, not a new CLI flag or automatic
memory-pressure policy. Omission preserves ordinary launches. PID 1 validates an
absolute loader-safe path and a regular file, appends the SO to existing
LD_PRELOAD, creates the socketpair and privately supplies its descriptor/inode.
The target must be able to read the SO and load its dependencies. Incompatible
preloads can prevent application startup. Static/secure-execution programs may
ignore preload; requests then fail without attempting injection.

Through the existing framed host-only guest control adapter:

```json
{"operation":"ReclaimAllocator","pid":23,"generation":4,"trim":true}
```

Use a live registered PID/generation (from Started for a control-launched process).
Control Start/Exec inherits the preload option and receives a separate channel.
`trim=false` checks the allocator without trimming. `trim=true` asks the persistent
worker to call malloc_trim(0). No executable payload or function address arrives
from the host. Inspect/trim replies contain status, pid, allocator and worker_tid;
trim also reports malloc_trim_result and RssAnon before/after (null if unavailable).
Zero from malloc_trim is valid and does not indicate a protocol failure.

## Thread and process lifecycle

The constructor registers an atfork child callback and creates one detached
pthread with a 256 KiB stack reservation and blocked application signals. It
restores the constructor thread's signal mask. The worker uses normal TLS and
allocator synchronization; original threads keep running. Between requests it
blocks on recv, with no timer wakeups or signal handler.

The SO requires the public malloc binding to equal __libc_malloc, together with
malloc_trim and gnu_get_libc_version. A distinct/interposed malloc binding is
rejected, even if a wrapper delegates to glibc. This is a conservative binding
check, not detection of every pool, namespace or later allocator switch. The SO
does not replace malloc/free. jemalloc/tcmalloc and runtime-specific pools do not
have reclaim backends in this version.

- **Ordinary fork:** the child validates and closes its inherited control socket.
  No allocator calls, locks or thread creation occur in the child callback.
  The parent keeps its worker/channel. Fork-only descendants are not registered
  reclaim targets. PID 1 must start a separate registered process to control it.
- **Fork/vfork followed by exec:** the constructor rejects a binding owned by the
  parent PID. It checks the socket inode before touching the inherited FD, so
  reuse of the old descriptor number cannot close an unrelated file. No child
  worker is created from the parent's channel.
- **Same-PID exec:** the channel intentionally has no CLOEXEC in the target.
  If the environment, preload and descriptor survive, the new constructor starts
  a new worker and PID 1 continues using the same registered channel/sequence.
  An exec in the middle of a request can lose the reply; requests are not retried.
- **Descriptor/environment removal:** control becomes unavailable. Clearing
  LD_PRELOAD or entering static/secure execution can also remove the worker.
- **Exit:** kernel process teardown releases the worker and target endpoint;
  normal PID-1 reaping drops the controller endpoint. Workers stop on EOF/error.
  The SO reserves its descriptor until exec/exit, including after worker failure,
  to avoid reusing it behind the fork callback. The callback additionally checks
  the inode in case application code closed/reused the descriptor itself.

The reference constructor relies on ordinary glibc fork handlers. Direct raw
clone/fork calls that bypass them must not use the inherited channel; such
noncooperative application behavior is not an independent control protocol.

## Failure behavior and memory limits

The fixed little-endian protocol uses 16-byte requests and 32-byte replies over
SOCK_SEQPACKET. PID 1 checks truncation, magic/version, request sequence, reported
PID and kernel SCM_CREDENTIALS, and closes/rejects unexpected passed descriptors.
The process registry serializes requests and fences PID reuse/reaping. Response
waiting has a two-second budget, not a real-time completion guarantee.

Any error retires the controller endpoint for that registration. Further requests
fail rather than consuming a late response. The application is neither stopped
nor killed. A trim already executing cannot be canceled and may still finish.
The cooperative channel does not protect against a target closing/replacing its
own descriptors, changing environment or libraries, or deliberately forging its
own results. No privileged callable interface is exposed to the target.

The SO, one socket and one worker remain resident per enabled process. Repeated
trims reuse them. malloc_trim may contend with allocations and add latency; it
does not flush every thread's tcache, collect a language heap or release live
allocations. Allocation patterns and fragmentation determine what can be freed.

malloc_trim(0) → guest free pages → virtio-balloon free-page reporting → VMM host
discard. On ordinary private anonymous Apple Silicon RAM, the host pauses vCPUs,
drains devices, unmaps HVF, replaces reported whole host pages with anonymous zero
mappings and remaps HVF before completing the report. File/COW and cold-pager
policies retain their own ownership. Reporting must be negotiated and is
asynchronous. Guest RSS alone is not proof of host physical-memory reduction.

## Validation

Run host contracts with `mise run test -- pvisor-guest`. On a Linux glibc host,
set the fixture paths and run ignored native tests through the same task:

```sh
PVISOR_RECLAIM_SUBJECT=/tmp/reclaim/subject \
PVISOR_RECLAIM_SO=/tmp/reclaim/libpvisor-reclaim.so \
PVISOR_RECLAIM_INTERPOSER=/tmp/reclaim/interposer.so \
PVISOR_RECLAIM_STATIC=/tmp/reclaim/static-subject \
mise run test -- pvisor-guest -- --run-ignored only -E 'test(allocator_reclaim)'
```

On Apple Silicon, the documented special runner is a disposable Linux pVisor VM.
Cross-build using `RUSTFLAGS='-C linker=rust-lld' cargo test --locked -p
pvisor-guest --target aarch64-unknown-linux-musl --no-run`, mount the fixture
output read-only and run the main-binary test executable with
`--include-ignored --test-threads=1 --nocapture` and the four environment paths.
Tests exercise the current control handler, process registry, real SO worker,
PID/generation checks and shared reaper. They do not boot the rebuilt PID 1 or
exercise actual vsock transport.

The subject touches/frees 64 MiB and checks live memory. Coverage includes fixed
and churning allocation workers, repeated trims/FD and mapping counts, CPU-only
work, blocked read integrity, fork while reclaiming, fork/exec with reused FDs,
vfork/exec, same-PID exec, closing/reusing the parent channel, interposed malloc,
static binaries, invalid packets/credentials/sequences and timeout retirement.

On 2026-11-07, all 33 main-binary tests passed in disposable aarch64 pVisor VMs
using Ubuntu 18.04 (glibc 2.27) and the available Ubuntu latest image (glibc 2.43).
On glibc 2.27 the four allocation threads plus main/worker case reduced anonymous
RSS from 67,555,328 to 454,656 bytes. These are functional checks, not a benchmark
or host physical-memory measurement. All 12 host tests, strict rustdoc,
aarch64/x86_64 clippy and affected pvisor consumer compilation passed. Both SO
architectures and x86_64 Rust test executables were cross-built; x86_64 was not
executed. The separate [whole-VMM memory experiment](../../benchmark/memory/allocator_reclaim_results.md)
boots the rebuilt PID 1 and uses real vsock on glibc 2.31: all 20 VMs passed
refill/live-data integrity checks, with measured host footprint reductions.
glibc 2.17 runtime coverage remains unverified.

References: [malloc_trim](https://man7.org/linux/man-pages/man3/malloc_trim.3.html),
[LD_PRELOAD](https://man7.org/linux/man-pages/man8/ld.so.8.html),
[pthread_atfork](https://man7.org/linux/man-pages/man3/pthread_atfork.3.html),
[Unix sockets](https://man7.org/linux/man-pages/man7/unix.7.html).
