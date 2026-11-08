# Low-load native catalog validation

## Scope and controls

This continues the requested 2026-11-08 01:04–09:04 UTC campaign after its PR
scope was narrowed to runtime/overlay work. The existing
`tests/network_cli_chaos.py` driver exercises public native network-catalog
commands against a new private store. It does not attach containers, deliver
packets, or configure the host network.

The model generates different command sequences from the current expected
catalog rather than repeating a fixed create/delete pair. It covers names,
CIDR/IPAM validation, ID selection, expected rejection without mutation,
duplicate concurrency, batch operations and interrupted-write recovery.

- One active driver; two CLI workers only for explicitly concurrent cases.
- Fifteen-second command deadlines, 600-second segments, 15-second gaps.
- Outer watchdog at 850 seconds plus 30 seconds termination grace; `nice -n 10`.
- Refuse work below 6 GiB available RAM or 20 GiB free disk, or above load1 8.
- At most 32 generated networks; all state and evidence are fixture-owned.
- No source changes, installs, external services, real-user catalog operations,
  global cleanup, unrelated-process signals or semantic-specification approvals.

Pinned read-only native binary SHA256:
`6da4ef6844ed2f36a9d7c0cc2172c77b576b99c0e354a296515b855785960f20`.
The worker records the exact executable path and argv in its durable evidence.
The existing driver and the published native runtime, not a task adapter, are
the subjects of this experiment.

## Completed segments

| Segment | UTC interval | Seed | Elapsed | Generated actions | CLI commands | Outcome |
| --- | --- | --- | --- | ---: | ---: | --- |
| 01 | 06:49:08–06:59:09 | 2026110823 | 600.078 s | 40 | 275 | Passed |
| 02 | 07:01:09–07:11:10 | 2026110824 | 600.049 s | 40 | 235 | Passed |
| 03 | 07:15:35–07:25:36 | 2026110825 | 600.047 s | 40 | 248 | Passed |
| 04 | 07:27:10–07:37:10 | 2026110826 | 600.054 s | 40 | 255 | Passed |
| 05 | 07:38:59–07:49:00 | 2026110827 | 600.022 s | 40 | 217 | Passed |
| 06 | 07:50:23–08:00:24 | 2026110828 | 600.039 s | 40 | 246 | Passed |
| 07 | 08:03:19–08:13:19 | 2026110829 | 600.041 s | 40 | 254 | Passed |
| 08 | 08:16:14–08:26:14 | 2026110830 | 600.054 s | 40 | 267 | Passed |
| 09 | 08:30:37–08:40:38 | 2026110831 | 600.039 s | 40 | 238 | Passed |

All nine catalog segments passed: 360 generated actions and 2235 CLI commands,
comprising 2009 successful exits, 198 expected rejection exits and 28 intentional
SIGKILL exits. There were no unexpected failures, timeouts, panics or retries.
The approximately 90 minutes of driver windows include the deliberate
15-second gaps; they are not 90 minutes of continuous CPU or network pressure.

Segment01 action counts: create15, interrupted recovery5, duplicate concurrency5,
remove3, list8, invalid3 and inspect1. There were 251 successful exits, 19
expected rejection exits and five intentional SIGKILL exits. No unexpected
failure, timeout, panic or retry occurred. The longest observed command was
14.73 ms; this is deadline-observation evidence, not a performance benchmark.

Segment02 action counts: create15, inspect5, invalid8, remove7, list3 and
interrupted recovery2. There were 212 successful exits, 21 expected rejection
exits and two intentional SIGKILL exits. No unexpected failure or timeout
occurred; the longest command was 13.865 ms. Its final catalog again contained
only `bridge` and `none`, with empty attachments and no temporary crash files.

Segment01's final authoritative catalog contained only `bridge` and `none`, with no
attachments. Four small interrupted-write temporary files were retained in
the private evidence store; they were not mistaken for committed catalog
entries or deleted by global cleanup.

Segment03 covered create13, list5, inspect6, invalid5, interrupted recovery4,
duplicate concurrency2 and remove5. Its 248 commands comprised 228 successful
exits, sixteen expected rejection exits and four intentional SIGKILL exits.
No unexpected failure, timeout or retry occurred. The longest command was
20.054 ms. Final catalog and attachments matched the same default-only state;
one 2120-byte private crash-temp file remains as evidence.

Segment04 covered create22, interrupted recovery4, duplicate concurrency4,
remove5, invalid3 and list2. Its 255 commands comprised 228 successful exits,
23 expected rejection exits and four intentional SIGKILL exits. No unexpected
failure, timeout or retry occurred. The longest command was 17.570 ms.
The final catalog again matched the default-only state; one 3030-byte private
crash-temp file was preserved.

Segment05 covered create17, list6, interrupted recovery3, remove7, inspect3,
invalid3 and duplicate concurrency1. Its 217 commands comprised 195 successful
exits, nineteen expected rejection exits and three intentional SIGKILL exits.
Segment06 covered create18, list4, invalid3, interrupted recovery3, duplicate
concurrency4, inspect5 and remove3. Its 246 commands comprised 220 successful
exits, 23 expected rejection exits and three intentional SIGKILL exits.
Neither run had an unexpected failure, timeout or retry. Longest commands were
20.968 ms and 17.679 ms respectively. Both ended at the default-only catalog
with empty attachments and no crash-temp remnants.

Segment07 covered create17, remove6, inspect5, invalid4, duplicate concurrency5,
list1 and interrupted recovery2. Its 254 commands comprised 224 successful
exits, 28 expected rejection exits and two intentional SIGKILL exits. No
unexpected failure, timeout or retry occurred; longest command was 16.695 ms.
The default-only final catalog and empty attachments were preserved, with one
1586-byte private crash-temp file retained.

Segment08 covered create20, duplicate concurrency4, list3, interrupted recovery4,
inspect1, invalid5 and remove3. Its 267 commands comprised 239 successful exits,
24 expected rejection exits and four intentional SIGKILL exits. Segment09
covered duplicate concurrency4, create15, remove6, list4, inspect6, invalid4 and
interrupted recovery1; its 238 commands comprised 212 successful exits,
25 expected rejection exits and one intentional SIGKILL exit.
Neither run had unexpected failures, timeouts or retries. Longest commands were
16.486 ms and 21.247 ms. Both finished with the default-only catalog and empty
attachments; private crash-temp files of 3090 and 1284 bytes were retained.

## Resources, cleanup and evidence

Segment01 minimum available RAM was 18,701,455,360 bytes, minimum free disk
339,418,841,088 bytes, and maximum load1 0.849. The native hash was unchanged
before/after. The owned driver group was gone, no readable exact-store CLI
process remained, mountinfo was unchanged, and the private HOME was empty.
Three same-UID process-environment permission denials were recorded; no
all-process or all-namespace absence claim is made.

Segment02 minimum available RAM was 18,312,196,096 bytes, minimum free disk
339,341,709,312 bytes, and maximum load1 1.2056. Its driver group and readable
exact-store processes were absent, private HOME was empty, and mountinfo and
the approved binary hash were unchanged. The same process-visibility limits
remain recorded; unknown resources were left untouched.

Segment03 minimum available RAM was 19,093,274,624 bytes, minimum free disk
339,355,455,488 bytes, and maximum load1 0.6362. Its driver group and readable
exact-store processes were absent; private HOME, mountinfo and binary-hash
checks passed with the same explicit visibility limits.

Segment04 minimum available RAM was 19,001,053,184 bytes, minimum free disk
339,347,509,248 bytes, and maximum load1 0.7822. Its owned group/readable
exact-store process checks, mountinfo, private HOME and binary-hash checks
passed, subject to the same recorded process-visibility limits.

| Segment | Minimum available RAM (bytes) | Minimum free disk (bytes) | Maximum load1 |
| --- | ---: | ---: | ---: |
| 05 | 18,990,575,616 | 339,466,735,616 | 0.4863 |
| 06 | 18,961,719,296 | 339,453,722,624 | 0.4341 |
| 07 | 19,034,312,704 | 339,457,282,048 | 0.5361 |
| 08 | 18,972,934,144 | 339,410,329,600 | 0.6880 |
| 09 | 19,011,289,088 | 339,405,971,456 | 0.5166 |

These additional runs passed all resource gates, retained the approved binary
hash and had no readable exact-store processes, owned groups, mountinfo changes
or private-HOME contents after completion. The three denied environment reads
remain a visibility limit, not evidence of ownership or permission to kill.

Detailed commands, resource samples, expected-model transitions and results are
retained in the runtime-catalog worker's `.data/runtime-catalog/segment01/`
and `segment01-supervision/` directories, with corresponding `segment02`
through `segment09` directories for subsequent runs. They are machine-local evidence,
not source/publication artifacts.

Each additional segment needs a new explicit workload grant, a different seed,
an exclusive private output/store, passing resource checks and the same artifact
hash. An unexpected result stops the driver for investigation; uncertainty is
not converted into a pass or automatic retry. No new segment starts after
09:04 UTC. The requested window completed at the actual final clock observation
documented below.

No segment10 is authorized. The remaining window is reserved for final
targeted checks and a delayed read-only audit of the nine known private stores,
owned groups, observable processes/mounts and the pinned artifact. Logged
mutating commands will not be replayed.

## Delayed read-only quiescence audit

The first two actual snapshots ran at 08:50:37.529 and 08:55:00.166 UTC.
All 36 constructed read-only queries passed: `network ls --no-trunc` and
`network inspect bridge none` against each of the nine known private stores
at each checkpoint. Every catalog still contained only the two built-ins with
empty attachments; catalog hashes were unchanged across queries and snapshots.
The approved binary and driver hashes matched at both checkpoints.

After all queries were reaped, all nine recorded process groups were absent,
no readable exact-store process or store mount remained, and mountinfo matched
all nine segment baselines. The audit excluded its own process and ancestors
by PID/birth identity. Two same-UID environment reads were denied per snapshot,
with zero observation races; those inaccessible processes remain unattributed.
The nine known private crash-temp files, totaling 17,644 bytes, were unchanged
and retained. No unknown resources were touched.

Resource gates passed: minimum available RAM 18,077,257,728 bytes, free disk
339,379,437,568 bytes, maximum load1 0.748. Evidence remains in
`.data/runtime-catalog/audit-phase1/`. This phase made no catalog mutations,
started no new generated segment and changed no source files. A separately
granted final read-only checkpoint established the actual window end below;
the phase-one result alone did not establish completion.

The final 18 read-only queries completed at 09:03:00.160 UTC, before the
09:03:30 cutoff. The 09:03:00.166 snapshot again found only `bridge` and `none`
with empty attachments in all nine stores. Catalog hashes and all nine
crash-temp files were unchanged from 08:55. Binary/driver hashes matched.
All nine known groups were absent, with no readable exact-store process or
store mount and unchanged mountinfo. Two same-UID environment reads remained
inaccessible; zero observation races occurred.

The final resource/clock observation was recorded at **09:04:00.000362 UTC**:
available RAM 18,309,582,848 bytes, free disk 339,344,244,736 bytes, load1 0.5034.
All final-phase gates passed; minima were RAM 17,901,273,088 bytes and disk
339,328,389,120 bytes, maximum load1 0.8374. Evidence is retained in
`.data/runtime-catalog/audit-final/`. No mutations, deletions, unknown-resource
cleanup, new segments or further workload followed. This closes the requested
eight-hour iterative campaign window, not eight hours of uninterrupted load.
