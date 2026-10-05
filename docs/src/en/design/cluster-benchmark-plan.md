# Cluster performance evaluation plan

Organize benchmarks as product decision → falsifiable hypothesis → controls → measurement → judgment. Plotting memory and startup curves alone does not establish pVisor scalability or commercial advantage.

The initial research scenario is Agent execution under a fixed host budget: short tool computation alternates with long model waits, tasks share resources, and Controller recovers through Worker reconciliation. This is a development hypothesis, not validated customer demand.

Updated direction: prioritize S1–S5 in [shared working sets and lazy loading](cluster/shared-working-set.md). Test whether common baselines cost once per group while tasks pay increments for actual access/private modifications, measuring first useful results and completion together. Independent-boot probes do not exercise these mechanisms; hit rates and earlier readiness cannot substitute for fixed-budget useful-work gains.

This protocol was written after the [2026-10-05 exploratory measurements](../benchmarks/cluster-scalability.md). **These questions were not registered before that data collection and cannot be claimed as hypotheses already validated by it**. This page designs future experiments; it adds no VM measurements or new PASS results.

## What the existing curves answer {#existing}

| Observation | Question it addresses | Unsupported conclusions |
|---|---|---|
| One to four guests, memory 93→343 MiB | Does adding a VM and Worker under minimal shell/sleep load cause abnormal plateau growth? No obvious superlinear growth was observed | Real Agent marginal memory, single-Worker density, startup peaks, long-term leaks or fixed-RAM capacity |
| Readiness P50 3.81→4.38 s; launch rate 3.45× | Can these lightweight tasks become ready in parallel when adding Workers and CPU budget? Yes | Fixed-machine completion throughput, superiority over other runtimes or low scheduling overhead; 86.2% is this probe's launch-efficiency ratio, not overall product scaling efficiency |
| Counts about 129–287 ns; million-history scan reference about 134 ms | Does maintained indexing avoid scanning all history? The measurement is consistent with that mechanism | Controller HTTP throughput or scheduling a million active tasks |
| Million records: about 6.39 GiB RSS and 16.09 s warm replay | Can historical retention create memory and recovery costs? Yes; further attribution is warranted | Net Controller memory, cold-recovery SLOs or benefits from a particular compaction design; RSS includes fixtures and replay has only one observation |

Retain these curves as exploratory and diagnostic evidence, rather than validation of fixed-budget density, production capacity or competitor advantage. The 3.8 s readiness interval needs decomposition. Existing data does not measure its phases, and it cannot be subtracted from VM-only startup measured under another configuration.

## Q1: Where does startup waiting occur? {#q1}

**Decision: optimize Controller, Worker preparation, VM startup or CPU quota first?**

Hypothesis: Cluster adds material queue/control/preparation costs under identical native configuration, accounting for part of user-visible readiness delay.

Compare direct native VM execution with execution through Cluster. Use the same pinned release artifacts, rootfs, firmware, policies, guest CPU/RAM, host cgroups and readiness markers. Start at concurrency one, then two and four. Separate cold preparation from prepared environments; vary CPU quota in an independent experiment. Do not compare the existing debug/half-core results with historical measurements using another configuration.

Record submission, intent commit, assignment send/receive, Worker preparation completion, native VM start and guest-ready for each task, plus cgroup CPU usage/throttling, queue length and failures. Document timing uncertainty across processes/hosts; future cross-host timestamps cannot be subtracted without calibration. Report total submission-to-ready latency and full task-completion latency separately.

Judge paired differences and phase distributions. If native preparation dominates, Controller optimization cannot solve the main problem. If scheduling queues grow with concurrency while direct execution does not, prioritize control/scheduling paths. Missing controls, insufficient timing quality or differences within measurement variation mean attribution remains unresolved, without a causal conclusion.

## Q2: What real memory cost does each additional Agent impose? {#q2}

**Decision: how should nodes be sized, and should memory work target VMM/guest, workspace sharing or offload?**

Hypothesis: common environments amortize shared costs and offload reduces physical occupancy while waiting, without restoration costs cancelling the gain.

Fix environments and per-task inputs. Measure idle guests, real tool tasks and verified fixed private working sets separately, at concurrency one, two and four. Separate multiple guests in one Worker from multiple Workers. Sample base Controller/Worker costs separately. Compare ordinary residency, pause at the same state and offload reversibly, keeping state, CPU budget and guest RAM constant.

Report base-service memory, plateau/startup peaks, native process identity and PSS, cgroup anon/file/shmem, configured RAM and actual working set. Also report offload storage writes, restoration/first-read latency and data validation. File-category memory cannot simply be deducted as reclaimable cache; host cgroup accounting, PSS and logical reservations have separate definitions.

Judge marginal cost and memory occupancy integrated over time, together with restored-task completion latency, CPU and I/O. Reducing one RAM proxy or plateau value does not establish an overall benefit. Current offload retains logical RAM reservations and slots, so reduced physical occupancy cannot directly establish higher admission capacity.

## Q3: How much more correct Agent work completes under fixed CPU/RAM budgets? {#q3}

**Decision: should CPU release during model waits be enabled by default, and does it create a saleable unit-cost benefit?**

Hypothesis: pausing and releasing logical CPU increases useful completion throughput for long model waits and short tool computation. CPU-bound workloads provide a negative control; equal gains cannot be assumed.

Freeze total host CPU/RAM caps, task sets and arrival traces. Compare ordinary execution against `release_cpu_on_idle` in the same implementation, using CPU-bound, model-wait-heavy and alternating tool/model workloads separately. Model responses and delays are deterministic; tools actually execute and artifacts are verified. Outputs, model delay and computation cannot change to manufacture gains.

At most four real guests run per arm; larger backlogs must queue. A possible experiment uses one Worker with four slots, a two-core budget and one-core logical reservations per task: ordinary execution admits at most two computing tasks; whether released waiting CPU admits more tasks is the question. RAM/slot limits remain effective. One Worker and four independent Workers cannot be treated as the same deployment.

Primary metrics are **correct tasks completed per unit time, fixed-input completion latency, CPU seconds/successful task, and physical memory occupancy integrated over time/successful task**. Also report provider waits, live VM counts, logical CPU reservations and actual utilization. State whether warm startup, evidence uploads, Controller, Gateway and model service costs are included. Failures, timeouts and unfinished tasks cannot disappear from denominators.

Judge paired gains under identical work and budgets, rather than VM counts or launch rate. Gains caused by increasing CPU budget, reducing work or ignoring delivery do not support the hypothesis. CPU shortages, worse tails and greater resident memory can offset throughput gains. Multiple paused VMs demonstrate a scheduling change without automatically making customer work faster or cheaper.

Define `R = throughput_idle / throughput_ordinary` in advance, with null hypothesis `R ≤ 1`. After the pilot and before formal sampling, freeze a practically meaningful minimum gain, the method for batch-level paired confidence intervals, and acceptable latency/memory costs. An interval crossing one means insufficient evidence; a point estimate above one alone does not establish an advantage. Even supported throughput gains need cost and customer constraints before deciding the default.

## Q4: Does task history slow current scheduling? {#q4}

**Decision: continue optimizing ready/poll hot paths, or prioritize history compaction and management interfaces?**

Hypothesis: with the active set fixed, hot-poll candidate work does not grow linearly with terminal history. Large ready sets, incompatible candidates and management queries can still affect tail latency and fairness.

First fix Worker count, active/ready sets and submission rate while increasing terminal history. Then fix history and independently vary ready sets, Worker count and incompatible-candidate proportion. Measure typed API and actual HTTP separately. Simulated protocol Workers are not deployed Workers and are not VM concurrency.

Report submission-to-assignment P50/P95, poll latency/CPU/candidate visits, maximum queued time, service RSS and allocator/fixture costs, and low-frequency intent/receipt journal writes, compaction and replay costs. The candidate bound from `queue_lookahead` is a local mechanism assertion, without promising constant total request time.

Distinguish history-driven scans, active-set work and memory/journal retention. Nanosecond counts queries cannot substitute for these measurements. Retain archived million-history results, but future bounded experiments must use sizes that fit their budget rather than rerunning excessive points to complete a curve.

## Q5: Does eventual reconciliation converge safely after restart? {#q5}

**Decision: does the simplified Controller preserve execution semantics at an acceptable recovery cost?**

Hypothesis: reachable Workers restore scheduling after restart without changing execution identity or restarting the same native execution; unreachable Workers retain pending state and reservations.

Restart Controller with at most four running/paused/delivering guests. Test normal Worker reports, delayed reports and temporary unreachability separately. Record process startup, journal replay, HTTP health, first correct state, reachable-Worker reconciliation coverage, confirmation of all reachable identities and restored admission for new work separately.

Check full lease keys/generations, execution-start counts in the fixture, resource/evidence references and cancellation/control delivery. Unreachable Workers cannot be declared recovered; old deadlines must not release resources or reassign unknown executions. Convergence time under permanent partition is undefined, rather than zero or a successful timeout.

Pass safety assertions before assessing recovery latency. A fixture with no duplicate execution does not establish exactly-once behavior in arbitrary external systems. Warm local replay is not full eventual-state convergence. Expected rejections such as complete corrupt journal frames must be classified separately from operational performance failures.

## Q6: Do resources and latency accumulate during sustained operation? {#q6}

**Decision: is this suitable for persistent Workers, and are artifact/history capacity policies sufficient?**

Hypothesis: with bounded history/evidence policies, repeated task creation, completion, cancellation and delivery do not continuously accumulate active processes, descriptors, mounts or running reservations.

Repeat fixed inputs with at most four simultaneous guests. Fix retention policy and compare early/middle/late batches and before/after GC. Distinguish retained evidence, retained task metadata, reclaimable page cache and native resources. Intentionally retained terminal records cannot all be classified as leaks.

Report live native identities, descriptor/mount counts, running reservations, service memory, file/journal bytes, startup and completion latency against cumulative task count. Label cancellation and unreachable samples separately; skipping exceptional paths cannot be used to flatten curves.

Judge convergence against lifecycle semantics and declared retention policy. Without bounded terminal retention, the long-term capacity hypothesis remains unvalidated even if 42 startup probes pass.

## Protocol to freeze before measuring {#protocol}

Save a question ID, intended product decision, hypothesis/counterexample, control version/configuration, primary metric formulas, secondary metrics, correctness assertions, workload/arrival trace/random seed, caps, sample counts/stopping rules, failure accounting and judgment criteria. Configuration or metric changes after freezing require a new protocol and explanation. Exploratory results cannot retrospectively become preregistered validation.

This page is a question/protocol draft. Formal workloads, sample counts, statistical methods and business thresholds are not frozen, so **it is not an executable acceptance-and-scoring protocol**. Formal experiments report supported, refuted or inconclusive hypotheses. Safety assertion failures remain failures and cannot be offset by performance gains. Unmeasured points within budget and insufficient statistical evidence are inconclusive.

Use pinned release binaries and source/firmware/rootfs identities. Alternate or randomize A/B order. Run a small pilot to estimate variability before freezing formal sample counts, and exclude the pilot from formal results. Retain raw batch data and report paired differences and variation separately. Tasks within one batch are not independent host repeats. Absolute customer latency/recovery SLOs are not yet specified; development screening targets must be declared before formal sampling instead of derived from results.

All experiments retain the four-real-sandbox maximum, explicit per-guest RAM/vCPU/CPU-time limits, kernel host CPU/memory caps, zero swap and available-memory preflight. Total budgets must remain fixed across A/B. Multiple guests inside one Worker require additional verification of per-guest and whole-tree limits; unsupported bounds mean unmeasured. Run comparison arms sequentially rather than launching eight guests for A/B.

Synthetic Controller/Worker protocol loads also need CPU/RAM caps and identified simulated identities. Larger record sets or sandbox counts do not automatically authorize extra resources. Stop near budgets according to declared rules and record unmeasured/failed points instead of weakening workloads or omitting results to obtain PASS.

## Priority and competitive conclusions {#priority}

Start with [shared RAM and environment lazy paths S1/S2](cluster/shared-working-set.md#experiments), together with **Q1 startup attribution / Q2 marginal memory**. Use S3 to explain concurrent misses, then validate combined gains in **Q3 fixed-budget useful-work throughput**. Q4/Q5 test control-plane design, and Q6 tests long-term capacity assumptions. Distinguish existing capabilities, required control harnesses and future proposals, and freeze protocols before execution.

Future Firecracker/runtime comparisons need identical tools, inputs, arrivals and total budgets, with isolation boundaries, distribution/image preparation costs and lifecycle capabilities reported separately. When matching is impossible, report deployment scenarios separately. Agent Env comparisons also need defined customer tasks and semantics; microVM startup, platform SLAs and Agent completion throughput do not form one ranking. Without matched measurements, competitor performance superiority remains unsupported.
