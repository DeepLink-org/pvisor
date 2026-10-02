# Known limitations

Report new boundary issues through [private disclosure](disclosure.md).

## Files and staging

| Limitation | Effect | Mitigation | Source |
| --- | --- | --- | --- |
| macOS symlink creation may return EPERM after creating the link | Return/effect mismatch | Review actual changes with `pvisor status --review` | S-STAGE-013, macOS XFAIL |
| Multi-file apply is not atomic against editors | Concurrent writes may interleave | Stop other writers; preflight conflict checks | [Staging](../concepts/staging.md) |
| Renames show delete+add | No explicit rename classification | Compare contents | [Staging](../concepts/staging.md) |
| Deny conservatively rejects multi-link files/new hard links | Some tools fail | Change applicable policy/execution choice | [CLI](../reference/cli.md) |
| Concurrent host mutation of lowers is outside file-rule guarantees | Rules may race | Trusted supervisor manages lowers | [CLI](../reference/cli.md) |
| Warn counts are access attempts, including metadata/cache effects | Not exact content-read counts | Treat as a signal | [CLI](../reference/cli.md) |
| macOS safe read documentation versus ambient evidence mismatch | Read confinement cannot be inferred | Trust Bundle; use VM for read boundary | October 2026 safe output, implementation confirmation pending |

## Credentials and sensitive files

| Limitation | Effect | Mitigation |
| --- | --- | --- |
| Renamed/embedded/Git-history secrets not detected by filename | Other secret forms readable | Custom rules; keep secrets out of workspace |
| Linux overlay deny does not hide original outside-view paths | Original secrets may be readable | `--filesystem sandbox`/VM |
| No bulk-read/tool-call attribution monitoring | Cannot infer mass-read-then-exfiltrate | Network boundary and Gateway records |

## Network

| Limitation | Effect | Mitigation |
| --- | --- | --- |
| Selective ordinary host/container proxy cooperative | NO_PROXY/direct sockets bypass | VM auto, host `--overlaynet-deny-all`, container offline |
| Observations cover intercepted traffic only | Absence does not prove no access | Mandatory boundary for stronger conclusions |
| Domains combine inference/telemetry/uploads | Authorized destinations may exfiltrate | Gateway and restricted routes |
| VM lacks UDP/IPv6/ICMP/QUIC/inbound | Dependent tools fail closed | Host execution with its limitations |

## Processes and executors

| Limitation | Effect | Mitigation |
| --- | --- | --- |
| No complete subprocess enforcement | `--strict` rejects everywhere | Validate fail-closed |
| Detached descendants outside group cleanup | May remain after exit | Linux namespaces/VM |
| Incomplete container controls | `--safe` rejects | Host/VM |
| macOS VMM retains host permissions | No hostile multi-tenancy | Do not isolate mutually untrusted tenants with it |

## Irreversible effects

Staging cannot undo APIs, databases, or messages. Checkpoints do not save memory/external state. Drop cannot undo prior apply batches.
