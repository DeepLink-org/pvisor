# Protocol, deployment and failure handling

One Controller and multiple Workers form the current minimum deployment. Configure service identities, storage directories and execution backends explicitly; validate reservations and execution isolation separately.

## Local startup and deployment boundaries {#deployment}

Build at the repository root, then start a Controller and a trusted host Worker in separate shells:

```sh
just cluster-build
export PVISOR_CLUSTER_TOKEN=local-admin-example-1234567890
export PVISOR_CLUSTER_WORKER_TOKEN=local-worker-example-1234567890
target/debug/pvisor-cluster serve --journal /tmp/pvisor-controller/journal
```

```sh
export PVISOR_CLUSTER_WORKER_TOKEN=local-worker-example-1234567890
target/debug/pvisor-worker --id worker-1 --state /tmp/pvisor-worker-1 \
  --backend host --slots 16 --memory-bytes 8589934592 --cpu-millis 4000
```

```sh
export PVISOR_CLUSTER_TOKEN=local-admin-example-1234567890
target/debug/pvisor-cluster submit crates/pvisor-cluster/examples/task.json
target/debug/pvisor-cluster show hello
target/debug/pvisor-cluster workers
```

Example credentials are for local reproduction; deployments use separate random credentials. The Controller listens on `127.0.0.1:19800` by default. Workers default to rootless and fail on unsupported backends. `host` explicitly means trusted process execution. Workers need unique IDs and exclusively owned state directories. Use `just cluster-build-gateway` for Gateway support.

Cross-host deployment requires a reachable Controller URL, TLS termination, persistent node state, environment/checkpoint repository permissions and executor platform dependencies. The shared Worker token authenticates a service role; it does not provide individual node/tenant authorization or isolation from malicious Workers. The tenant field is not an authenticated identity.

## Configuration and bounds {#configuration}

| Configuration/bound | Default or limit | Owner |
| --- | --- | --- |
| Controller URL | `http://127.0.0.1:19800` | `--url` / `PVISOR_CLUSTER_URL` |
| Admin / Worker tokens | Distinct values, each at least 16 characters | `PVISOR_CLUSTER_TOKEN` / `PVISOR_CLUSTER_WORKER_TOKEN` |
| Journal | `.pvisor/cluster/journal` | serve `--journal` |
| Lease duration | 30,000 ms; accepts 100–300,000 ms | serve `--lease-ms` |
| Metadata quota | 1 GiB; must accommodate a 16 MiB frame | `--max-journal-bytes` |
| Artifact payload cap | 8 GiB | `--max-artifact-bytes` |
| Object storage policy / tenant quotas | Explicit JSON files | `--artifact-limits` / `--quotas` |
| Ready window / assignment batch / task history | 256 / 64 / 1,000,000 | SchedulerConfig; not all fields exposed through CLI |
| HTTP scheduling request body | 4 MiB; upload has separate chunk limit | Server |
| Worker state / poll interval | `.pvisor/worker` / 1000 ms | `--state` / `--poll-ms` |
| Worker slots / RAM / CPU | 16 / 8 GiB / 4000 millis | `--slots` / `--memory-bytes` / `--cpu-millis` |
| Worker profile | Host-owned TOML; unknown fields rejected | `--config` |

Worker profiles configure Gateway, VM/container, networking, read-only lowers, admission, environments, checkpoint storage, CPU QoS and memory/CPU sampling. Node configuration owns secrets and provider credentials; advertised capabilities must match usable configuration.

## HTTP protocol directory {#api}

Shared types reside in `pvisor-core/src/cluster.rs`; current `CLUSTER_VERSION = 1`. GET and POST distinguish reads from submissions where paths coincide. All paths below have `/v1` prefixes unless explicitly stated.

| Role | Method and path | Request/response purpose |
| --- | --- | --- |
| Public | `GET /health` (no `/v1`) | Protocol version and Dispatcher availability; does not prove Worker health or storage headroom |
| Admin | `POST /tasks`, `GET /tasks/{id}` | TaskSpec → TaskRecord; inspect pending/terminal state |
| Admin | `POST /tasks/{id}/cancel`, `POST /tasks/{id}/resolve-lost` | Cancel; exact LeaseKey resolution of pending identity |
| Admin | `POST /tasks/{id}/control`, `GET /tasks/{id}/inference-wait` | ControlRequest; observe automatic waits without authorizing reply delivery |
| Admin | `POST /graphs`, `GET /graphs/{id}`, `POST /graphs/{id}/cancel` | Atomic DAG submission, queries and cancellation |
| Admin | `POST /tasks/{id}/forks`, `GET /tasks/{id}/forks/{request_id}` | Sealed-fork requests and creation receipts |
| Admin | `POST /tasks/{id}/live-forks`, `GET /tasks/{id}/live-forks/{request_id}` | Live capture/fork progress and receipts |
| Admin | `POST /environments`, `GET /environments/{digest}` | Immutable template registration/read |
| Admin | `GET /workers`, `POST /workers/{id}/drain`, `GET /counts` | Nodes, drain and derived counts |
| Admin | `GET /tasks/{id}/artifacts`, `GET /artifacts/{digest}` | Retained manifests / objects |
| Admin | `POST /tasks/{id}/artifact-downloads`, `POST /artifact-downloads/{id}/renew`, `POST /artifact-downloads/{id}/release` | Explicit protection for multi-object downloads |
| Admin | `GET /artifact-storage`, `POST /artifact-storage/limits` | Unique object space/counts, publication reservations and online policy |
| Admin | `POST /artifact-storage/gc/plan`, `POST /artifact-storage/gc/apply` | Preview/apply immutable reclamation plans |
| Worker | `POST /workers/register`, `POST /workers/poll` | WorkerRegistration; PollRequest → PollResponse |
| Worker | `POST /workers/recover`, `POST /workers/decline` | Old terminal-identity recovery; unstarted rejection |
| Worker | `POST /workers/native-done`, `POST /workers/complete` | Native teardown handoff; Completion and terminal receipt |
| Worker | `POST /workers/control-ack`, `POST /workers/inference-wait` | Native observations; begin/ready/observe barrier |
| Worker | `POST /workers/memory`, `POST /workers/cpu`, `POST /workers/node-memory` | Lease-bound observations / node sampling |
| Worker | `POST /workers/artifacts/{task_id}/{generation}/{worker_id}/{incarnation}/{digest}` | Exact-lease object upload |

Role tokens use Bearer headers. Invalid credentials return 401, domain conflicts commonly return 409, retired evidence returns 410, quota refusals return 507, and overload/uncertain commits/retryable publication failures return 503. Oversized bodies are refused by the HTTP framework. Unknown-task queries also use the current domain-error contract; do not assume every conventional REST status mapping.

Retry by identity and content, not request count. Task/graph IDs bind identical immutable specifications, controls/forks bind the same request ID, and completions bind exact keys and consistent evidence. An unchanged protocol version does not mean older binaries understand every new enum or transaction. Upgrade the Controller before enabling new controls, environments or waits; do not downgrade a writer against a new log without validation.

## Failure handling {#runbook}

| Symptom | Current behavior | Action |
| --- | --- | --- |
| HTTP timeout/lost response | Queued operation may have committed | Retry/query with the same identity; do not immediately create replacement execution |
| Controller restart | Nonterminal leases pending; reservations retained | Original Workers continue polling; inspect pending clearance |
| Controller outage exceeds local lease | Worker watchdog requests stop | Verify terminal delivery and effects; a stop request is not observed termination |
| Worker restart | Known terminal delivery first; unknown old execution not adopted | Preserve state; resolve pending identities if needed before new incarnation registration |
| Long-running pending state | Old Worker unreachable, GC blocked | Inspect node/business effects, then explicitly resolve-lost using the complete key |
| Controller quota full | Renewals/reads may continue; new intents/first results may fail | Inspect disk and raise metadata quota; preserve outbox evidence |
| Uncertain journal write/fsync | Poison, 503 | Inspect storage, preserve evidence, restart/verify; never silently truncate complete corruption |
| Artifact quota full | Publication refused, delivery pending | Preview retirement/GC and inspect plan or raise policy; resolve reconciliation first |
| Drain / SIGTERM | Drain prevents new reservations; Worker attempts cancellation/delivery | Wait for results and outbox convergence; SIGKILL is unconfirmed loss |

Restart preserves the journal and matching artifact authority/store. A partial live-directory copy is not a consistent backup, and arbitrary Worker cache/source-checkpoint deletion is not safe reclamation. Automated consistent backup/restore is not implemented.

Observable state includes TaskRecord phase/pending/lease/result/control history, Worker reserved/admission, counts, artifact-storage and native Worker evidence. Capacity monitoring should cover log bytes, object bytes/counts, pending duration, outbox backlog and node pressure. Unified metric export, alerting services and billing remain integration work.

## Validation matrix {#validation}

| Layer | Entry point / existing coverage | Conclusions it cannot establish |
| --- | --- | --- |
| Shared protocol and scheduler | `just test pvisor-cluster pvisor-core`: idempotency, fencing, DAGs, controls, quotas, replay, GC and inference waits | Native isolation or cross-host density |
| HTTP and independent Workers | `just test-cluster`: real process execution, Controller restart, same-key reconciliation, lost ACK/outbox | Exactly-once external effects |
| Gateway feature | `just test-cluster-gateway`: wait barriers, model/tool protocols and recovery boundaries | Real-provider performance or every hardware path |
| Linux VM/environment | `just test-cluster-vm`: explicit KVM/FUSE hardware gates | Migration across arbitrary hosts/architectures |
| VM + Gateway | `just test-cluster-vm-gateway`: real guest waits, manual pause, CPU readmission, brief server restart | Abrupt process crashes, long outages or cross-host disaster recovery |
| Node-quota experiments | `just test-cluster-cgroup`: finite Linux cgroup quota and controlled overcommit | General throughput/cost improvement |

Restart reconciliation tests keep real Worker execution alive beyond the persisted historical deadline, reopen the Controller, reconfirm the same key and check a once-only execution marker. They validate identity and absence of heartbeat writes. Group-commit tests validate shared syncs, queues/thresholds and uncertain commits. Hardware gates require devices and explicit execution; skipping them in default tests is not validation.

Semantic specifications follow the project's semspec process; passing tests and human approval are distinct. Design documentation does not replace measurements, semantic approval or production acceptance, and historical test totals do not establish new-version scale.

## Evolution and constraints {#evolution}

| Direction | Constraints to preserve |
| --- | --- |
| Further reduce Controller persistence | Define authority for upstream desired state, Worker terminal inventories and retained manifests; first solve unassigned intents and acknowledged history reconstruction |
| Multiple shards / HA | Stable shard ownership, routing and execution/object authority fencing; local file locks are not cross-node leader election |
| Online metadata compaction / Worker GC | Preserve idempotency IDs, receipts, lineage and active roots; define retirement/history contracts first |
| Finer-grained GC | Establish verifiable roots for unknown leases before narrowing the shard-wide pending gate |
| Node/tenant identities and remote CAS | Credential lifecycle, tenant authorization, object ownership and transport protection; shared tokens are insufficient |
| Scheduling and RAM reclamation | Define reusable capacity through native/node evidence; adjust reservations/overcommit after controlled experiments |
| Cross-host recovery and RL lifecycle | Pin runtime compatibility matrices, validate partitions/restarts/publication failures and coordinate scaffold/training state |
| Production scale | Multi-host long-running tests, density/useful-work/tail-latency benchmarks and operational/failure cost measurements |

Preserve [state authority](state-and-recovery.md#authority) and [design invariants](index.md#invariants) when extending the system. Workload research and open questions are covered in [cluster execution research](../research/cluster-execution.md).
