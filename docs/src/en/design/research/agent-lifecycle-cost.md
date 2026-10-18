# Agent task lifecycle cost

**pVisor aims to establish a new execution model for Agents: Agents retain their sequential programming model, while a shared execution engine takes over supported I/O, reclaims resources during waits, and resumes the original execution timeline when events are ready.**

Scaling autonomous Agent execution changes how resources must follow tasks. Faster sandbox startup is the foundation. The larger opportunity is keeping many stateful Agents progressing without reserving a complete resident environment for each timeline from start to finish.

## An Agent's timeline is continuous; its resource demand is intermittent {#waiting-cost}

A Coding Agent typically follows its own timeline: read a repository, request a model response, edit code, run tests, and choose the next step from the results. Tools, child processes, and background services may run concurrently, while the task retains state across turns.

Computation and waiting alternate along this timeline. While the Agent waits for a model response, inference happens in an external service, but its sandbox may still retain memory, caches, and process state.

DeepSeek's **DSec (DeepSeek Elastic Compute)** report provides production evidence: across one week of early-2026 samples, about 90% of sandboxes on both the container and microVM backends averaged at most 5% of their requested CPU. Median lifetimes were 17.4 and 15.5 minutes, and both P99s exceeded three hours. The report also describes sandboxes waiting for the model's next action while retaining state and caches. [DSec §1 and §4.3, Figs. 5 and 7](https://arxiv.org/html/2609.22978v1#S4.SS3)

![DSec production statistics and schematic Agent phases: long lifetimes, sparse CPU use, and potentially continuous state residency](assets/agent-workload-phases.svg)

These observations support **intermittent computation over long lifetimes**. Computation and waits recur across interaction turns without a fixed period. The phase timeline above is illustrative, not a measured DSec trace.

As concurrency grows, residency during waits accumulates. **Continuous logical execution need not require continuous physical resource occupancy.** The unit to optimize is therefore the Agent's complete lifecycle, from receiving an environment to delivering a result.

## A familiar transition in I/O execution models {#io-models}

High-concurrency servers have faced a similar change in resource organization. On a single thread processing requests sequentially, one request waiting for I/O delays later requests. A thread per connection preserves sequential code while allowing several timelines to overlap. When many connections wait, however, each retains a thread stack and execution context.

![From sequential processing on one thread to a thread per request: more concurrency, but waiting threads retain their execution contexts](assets/io-thread-timelines.svg)

Event-driven execution centralizes waiting, so fewer threads serve many connections. Reactors wait for readiness notifications such as epoll. Proactors submit an operation and wait for its completion, as IOCP does. Adopting these models directly often requires splitting sequential code into state machines or callbacks and managing more execution state. [epoll](https://man7.org/linux/man-pages/man7/epoll.7.html), [IOCP](https://learn.microsoft.com/en-us/windows/win32/fileio/i-o-completion-ports)

Transparent coroutines concentrate this adaptation in the runtime. Programs continue to express logic sequentially, while syscall hooks or standard-library integration hand supported blocking I/O to an event engine. The runtime suspends the coroutine during the wait and resumes it at the original call when ready. For example, libco combines socket-function hooks with epoll / kqueue so business logic can retain synchronous calls while executing asynchronously. [libco](https://github.com/Tencent/libco), [Go runtime scheduling](https://go.dev/doc/faq#goroutines)

![Reactors and Proactors share an event engine; transparent coroutines preserve sequential timelines above it, with runtime-managed suspension and resumption](assets/io-events-coroutines.svg)

The figure uses one worker to illustrate sharing; implementations can use several. A Reactor signals that I/O can be attempted; a Proactor signals completion of a submitted operation. Coroutines let applications avoid splitting their state around those events. Transparency applies to integrated I/O paths and preserves call semantics there. Arbitrary multithreaded programs still need explicit migration.

**pVisor applies this transition to Agent execution.** Agents keep their existing code and Linux tools; the execution engine turns supported waits into event-driven progress and resumption. Adaptation stays in the runtime, so no Agent needs a concurrency rewrite.

## Use the VM to hold the Agent's execution context {#agent-iocp}

Following the coroutine model, pVisor extends the scheduling unit to the Agent's entire execution environment. A coroutine retains its call stack and local variables. A VM holds the Agent's processes, memory and Linux kernel state, allowing the runtime to preserve and restore its execution context outside the application.

![The mechanism: coroutines adapt waits at call boundaries; pVisor takes over I/O at Linux virtual-device and protocol boundaries, preserving sequential application calls](assets/agent-vm-coroutine-bridge.svg)

**Transparency to the Agent means running its existing code, using its existing Linux tools and keeping its existing blocking calls.** File and network I/O crosses Linux and virtual devices into the host, where pVisor takes ownership on integrated device and protocol paths. Changes to the guest kernel, VMM and host services are concentrated at this layer. In the target design the Agent needs no asynchronous rewrite and no explicit suspend, offload or resume interface. [VM execution path](../vm-runtime.md)

## The Agent sees continuous execution; the host schedules events {#execution-path}

Consider a model request on a supported proxy path. The Agent issues the request and waits as usual. pVisor hands the request to the host, which advances it. When the entire guest can safely wait, pVisor retains its context and reclaims offloadable memory, making reusable resources available to other Agents. When a response arrives, pVisor restores that context and returns from the original call. The Agent proceeds to handle the result.

![Two views: Agent A waits for its original call; the host advances I/O and runs B and C during the wait, then restores A to resume in place](assets/agent-transparent-scheduling.svg)

This places several Agent timelines under a shared execution engine: **each Agent retains its logical progress, while pVisor schedules its physical resource occupancy.** Resumption continues the same process and execution attempt. The VM preserves machine context across the wait; the host retains the I/O state needed to keep the operation progressing.

Transparency is bounded by the original call's semantics: results, errors, file ordering and synchronization requirements must remain intact. The host must own independent requests and data before removing I/O's dependency on guest RAM. Model requests also require protocol integration; ordinary TCP forwarding cannot automatically take over a TLS session inside the guest. A guest with runnable background work continues executing.

Bulk writes require the same ownership transfer. Once the host owns independent write data, it can queue, coalesce and persist it. While preserving ordering, errors and synchronization semantics, an entire guest waiting only for those operations may become eligible for offload. **A smaller host buffer can release a larger guest memory footprint, which makes asynchronous I/O at scale worth pursuing.** Queue backpressure, synchronization and runnable background tasks affect the eligible window, and the buffer itself counts toward resource cost. [Agent I/O completion](../experimental/agent-io-completion/index.md)

A blocked vCPU already lets the host schedule other work. The additional capacity opportunity here comes primarily from reclaiming physical memory still retained by a waiting guest.

## Estimate capacity gains before completing the system {#capacity-estimate}

**For workloads with substantial waits and memory-limited capacity, roughly twice the steady-state capacity is a reasonable project target, reaching about 2.7 times in a high-wait scenario.** This target combines public prototypes, existing offload measurements and explicit parameter assumptions. Production capacity at this level has not been measured for the complete I/O handoff path.

### What public results and existing measurements establish {#capacity-evidence}

| Evidence | Observation | Role and limits in the estimate |
|---|---|---|
| [The Cost of Dynamic Reasoning, v2, §IV-A, Fig. 5](https://arxiv.org/html/2506.04301v2#S4.SS1) | Across four benchmarks and several Agent workflows, LLM inference averages 69.4% of total latency and tools 30.2%; the default is Llama-3.1-8B with vLLM 0.6.6 | Model stages can occupy most of the timeline; overlap with tools prevents treating this directly as whole-guest offload eligibility |
| [AgentSysBench, v1, §8.3, Fig. 25(c,d)](https://arxiv.org/html/2608.15127v1#S8.SS3) | Its Mini-SWE prototype offloads sandboxes after planning exceeds a time threshold and restores before the next tool step; average/peak resident memory falls to 1/4.6 and 1/2.1 of baseline, with latency increasing by at most 0.5% | Direct evidence for the potential of offloading during waits; memory is the aggregate across concurrent sandbox processes in this run, without establishing node density or whole-VM offload |
| [pVisor historical static memory measurements](https://github.com/DeepLink-org/pvisor/blob/main/benchmark.new/memory/README.md#published-curves-and-archived-history) and underlying readings | Linux x86_64/KVM, 512 MiB guest, 64 MiB application working set; complete product-process PSS falls from about 179–181 MiB to 36 MiB, about 80%; product-group cgroup memory including cache falls from about 224 MiB to 19 MiB | A reference for retained memory after offload; N=1 per condition and a five-second window, from a historical batch, rather than a real-Agent or node-capacity measurement |

AgentSysBench omits the threshold value, concurrency and underlying offload mechanism, so it establishes feasibility for the direction but not the parameter values below. pVisor's PSS and cgroup readings use different accounting; they cannot be added or substituted for net node-wide reclamation.

Public evidence also shows workload variation. In [AgentCgroup, v2, §3.2](https://arxiv.org/html/2602.09345v2#S3.SS2), LLM reasoning occupies only 26–44% of end-to-end SWE-rebench execution, with the remainder including tools and initialization. Offload eligibility therefore needs workload-specific assumptions. DSec's low average CPU usage likewise cannot establish that 95% of the timeline is offloadable.

### Derive capacity from eligible windows {#capacity-model}

Choose a workload and a consistent physical-memory accounting scope, then define four quantities:

- `m0`: average resident memory per Agent in the baseline, using actual physical usage rather than configured guest RAM.
- `f`: the fraction of baseline memory-time covered by effective offload windows. With roughly constant baseline residency, this is the safely offloadable time fraction. Exclude background work, unhandled I/O, short waits and offload/restore transitions.
- `r`: the fraction of baseline residency actually released inside those windows, weighted by their baseline memory-time. Retained VM memory is already deducted here.
- `h`: average additional host residency over the full observation window, divided by `m0`, including request/response buffers, completion queues, unmapped page cache and extra handoff copies. Account for fixed shared services separately in the node budget.

Over comparable observation windows:

```text
m1 / m0 = 1 - f * r + h
K_memory = m0 / m1 = 1 / (1 - f * r + h)
```

Using `r = 80%` as a reference assumption and `h = 5%` as a budget target for additional host residency gives three scenarios. **These are assumptions and calculated results, rather than measured pVisor Agent capacity.**

| Scenario | Effective offload fraction f | Reclaimed fraction within windows r | Added host overhead h | New average residency m1 / m0 | Memory-limited steady-state capacity multiplier |
|---|---:|---:|---:|---:|---:|
| Conservative | 40% | 80% | 5% | 73% | 1.37 times |
| Central target workload | 70% | 80% | 5% | 49% | 2.04 times |
| High-wait workload | 85% | 80% | 5% | 37% | 2.70 times |

The central scenario is `1 - 70% × 80% + 5% = 49%`. An Agent averaging 1 GiB before optimization would average about 0.49 GiB under the same accounting. After reserving memory for the system, shared engine and restoration headroom, the same 100 GiB Agent budget would cover the average residency of about 100 → 204 execution contexts.

`f = 70%` assumes most relevant waits can be captured safely. It is a design assumption, not a rounding of the published 69.4%. `r = 80%` takes its scale from small-working-set offload, with actual results depending on working sets, sharing and caches. `h = 5%` is a budget awaiting implementation validation: with 1 GiB of baseline residency it allows about 51 MiB of average additional residency. Smaller guests or large response backlogs require recalculation.

The same model evaluates buffer spending. Holding `f = 70%` and `r = 80%`, increasing `h` from 5% to 15% or 30% lowers the multiplier from 2.04 to 1.69 or 1.35. The experimental host write buffer currently caps payload at 64 MiB per connection. A cap differs from average occupancy; multiple connections, metadata and response spooling must share a resource budget. [Buffer budget and current limits](../experimental/agent-io-completion/index.md)

### Include restoration and peaks in the cost {#capacity-limits}

The node's average-residency planning quantity is:

```text
N_memory = floor((R_node - S_fixed - P_headroom) / m)
```

`S_fixed` covers the system and shared services; `P_headroom` reserves memory for active peaks, simultaneous restoration and temporary copies. The table's multiplier assumes the same available budget before and after optimization. Additional shared-engine memory or restoration headroom requires multiplying by the ratio of the new available budget to the old. Staggered waits, restoration admission and hard budgets determine deployable capacity.

Offload and restoration can also extend task duration. In the same historical static batch, raw offload takes about 44–98 ms. The subsequent complete task—including restoration, a 64 MiB digest and write/fsync/read-back verification—takes about 156–178 ms longer than without offload. These are single observations taken during background compilation; they are not isolated resume latency or a tail-latency guarantee. [Timing definitions and data](https://github.com/DeepLink-org/pvisor/blob/main/benchmark.new/memory/README.md#published-curves-and-archived-history)

If end-to-end task duration increases by a fraction `delta`, calculate average residency over the complete task in each implementation. Memory-time cost per completed task changes by:

```text
C_memory_time_new / C_memory_time_old = (m1 / m0) * (1 + delta)
```

`m1` must include residency during restoration and queueing. If delay changes the phase distribution, recalculate `f`, `r` and `h`. For example, additionally assuming the central scenario's complete-task average residency ratio remains 49% while tasks take 5% longer gives `49% × 1.05 = 51.45%`, saving about 48.6% of memory-time. This estimates memory cost on the Agent execution side. Node-count savings also depend on CPU, storage, network and model quotas; model-token charges remain separate.

Restoration must also satisfy “wakeups per second × bytes actually read back per wakeup ≤ available restoration bandwidth,” with an equivalent constraint for offload writes. Correlated I/O completions require queued restoration, and that queueing counts toward end-to-end latency. Report steady-state capacity, task throughput and restoration tail latency separately.

### Compare against existing VM optimizations {#vm-baseline}

Existing VM systems also optimize memory. On a real agentic-RL workload, DSec's virtio-pmem + DAX reduces peak host memory by 40.2%. DAMON + balloon free-page reporting alone reduces time-integrated memory by 21.2%, with little peak reduction. [DSec, v1, §8.4, Fig. 12](https://arxiv.org/html/2609.22978v1#S8.SS4)

These mechanisms reduce duplicate caches and idle pages. pVisor's core challenge is to let the host own I/O that must keep progressing, expanding the effective whole-guest offload fraction `f` while controlling additional overhead `h`. A microVM can supply pause, offload and restore primitives; this path also needs I/O ownership transfer, completion events and restoration scheduling.

Subsequent comparisons should retain existing sharing and reclamation, then measure incremental gains with the same hardware, image, workload, correctness and latency constraints. DSec's two improvements, pVisor's static reclamation ratio and the capacity estimate above have different baselines and cannot be added or multiplied together.

## Complete one transparent wait path first {#validation}

Start with one I/O path and complete the loop: take ownership → offload safely → advance on the host → wake on an event → return from the original call. Then extend to streaming and more tools. Validation must check call semantics and whether fixed resources support more useful execution after counting host buffers, offload and restoration costs.

The prototype should progressively replace the estimate's assumptions with observations:

| Data to record | What it establishes |
|---|---|
| Baseline residency curves, shared services and node-wide caches | `m0` and the available node budget; count shared pages once |
| I/O handoff, RAM-reference drain, actual reclamation and restoration-start times | Effective fraction `f`; transitions do not count as reclaimed time |
| Retained memory, host buffers, queues, page cache and temporary copies | Baseline-residency reclaimed fraction `r` and added overhead `h`, accounting for memory transferred to another owner |
| Simultaneous wakeups, read/write bytes, active/restore peaks and the latency distribution to first useful execution | Whether headroom, bandwidth and queueing support the estimated capacity; tail latency requires enough samples |
| Original-call outputs, errors, ordering, synchronization, cancellation, task completions and end-to-end latency | Whether additional execution contexts preserve correctness and complete useful work |

Existing foundations include controlled file and network paths, pause and RAM-offload primitives, and an experimental host write buffer. Automatic wait detection, I/O handoff, completion-driven wakeups and resumption scheduling still need end-to-end integration and validation. [Wait and offload experiment](../experimental/vcpu-idle-offload/index.md), [Agent I/O completion](../experimental/agent-io-completion/index.md)

**Transparent coroutines are the closest precedent: sequential code, shared resources, runtime-managed waits. pVisor applies that model to Agent execution, with the VM as the context and control boundary.**
