# Known limitations

Known gaps are listed with their impact, source, and mitigation. Report new findings through the [vulnerability disclosure policy](disclosure.md).

## Files and staging

| Limitation | Impact | Mitigation | Source |
| --- | --- | --- | --- |
| macOS may return EPERM after creating a symlink even though the link exists | The return value disagrees with the effect, so an agent may mistake success for failure | Trust the actual changes in the review list | Semantic spec S-STAGE-013 (XFAIL on macOS) |
| Multi-file apply is not atomic against external editors | External writes during apply may interleave with the batch | Stop other writers during apply; conflict checks run before writing | [Staging and apply semantics](../concepts/staging.md) |
| Renames appear in the review list as a deletion plus an addition | Review cannot tell that a rename happened | Judge by content | [Staging and apply semantics](../concepts/staging.md) |
| With deny rules enabled, multi-hard-link files and new hard links are conservatively rejected | Tools that rely on hard links may fail | Disable the rule for that tool or use another execution path | [File access rules](../reference/cli.md) |
| File rules do not promise to resist a race with another host process rewriting the underlying file | Rules may fail when the lower layer is rewritten concurrently | Leave the lower directory under a trusted supervisor | [File access rules](../reference/cli.md) |
| `warn` counts filesystem access attempts, not content reads | Counts are imprecise; the kernel cache may merge accesses | Treat them as a signal only | [File access rules](../reference/cli.md) |
| macOS host `--safe` is documented as enforcing a Seatbelt read scope, but Run evidence reports reads as ambient | Documentation and evidence disagree, so the read boundary must follow the evidence | Read the Safety boundary in `status --review`; use the VM when you need a read boundary | October 2026 measured `--safe` Run output; pending implementation confirmation |

## Credentials and sensitive files

| Limitation | Impact | Mitigation |
| --- | --- | --- |
| Filename rules do not detect renamed copies, keys in source, or content in Git history | Secrets may be read in other forms | Add rules for your project; keep secrets out of the workspace |
| On Linux, overlay deny rules do not hide secrets at their original paths outside the workspace view | Secrets outside the view may still be readable | Use `--filesystem sandbox` or the VM |
| No bulk-read or tool-call correlation monitoring | Cannot identify "read many files, then exfiltrate" | Combine with network boundaries and Gateway records |

## Network

| Limitation | Impact | Mitigation |
| --- | --- | --- |
| Selective proxying on host and container is cooperative | Clients that ignore the proxy, add `NO_PROXY`, or open raw sockets can bypass it | VM `auto`, host `--overlaynet-deny-all`, or container offline mode |
| Network observations cover only requests that pass through the proxy | A destination absent from the records was not necessarily never reached | Use a mandatory boundary when you need a complete conclusion |
| Domain rules cannot distinguish inference, telemetry, and upload APIs on the same domain | An authorized destination may be used to exfiltrate | Use a Gateway and restrict routes |
| The VM data plane does not support UDP, IPv6, ICMP, QUIC, or inbound forwarding | Tools that depend on those protocols fail (fail-closed) | Fall back to host execution |

## Processes and executors

| Limitation | Impact | Mitigation |
| --- | --- | --- |
| No executor claims complete subprocess enforcement | `--strict` currently refuses to run on every executor | Use `--strict` to verify fail-closed behavior |
| Descendants that actively leave the process group are outside process-group cleanup | Processes may survive after the Run ends | Use Linux namespaces or the VM |
| The container executor does not claim complete capability enforcement | `--safe` refuses to start on container | Use host or VM |
| On macOS the VMM holds the calling user's host permissions | The VM is not a hostile multi-tenant boundary | Do not use it to isolate mutually untrusted tenants |

## Irreversible effects

Staging does not undo remote API calls, database writes, or messages already sent; logical checkpoints do not save process memory or external service state; `drop` does not undo a batch that has already been applied.
