# Security and evidence model

PolicyVisor does not compress security into one `safe` or `sandboxed` label.
Every Run reports guarantees by capability dimension. pVisor owns admission
and runtime enforcement. Placement and recovery mechanisms do not upgrade a
pVisor evidence level. Configured capture preserves lifecycle facts and only
the Evidence carried by Gateway or lifecycle event records. The full Run Bundle
evidence inventory remains local.

| Dimension | Example mechanism | Evidence question |
| --- | --- | --- |
| Filesystem read | synthetic root, allowlisted projection | Which host paths were visible? |
| Filesystem write | staged OverlayFS, Landlock, Seatbelt | Where could the process tree write? |
| Network | private namespace, virtio-net, proxy policy | Could direct sockets bypass policy? |
| Process | namespace, sandbox profile, inherited-FD cleanup | Which descendants shared the boundary? |
| Credentials | Explicit environment delivery | Which identity received and used the secret? |
| Effects | stage, apply and drop records | Which consequences reached the real system? |

The following evidence vocabulary describes increasing claims. It is not a list of guarantees implemented by every executor; “Attested” does not imply a shipped cryptographic attestation service.

Evidence has four useful levels:

1. **Declared** — configuration requested a boundary.
2. **Mediated** — an Agent-facing path passed through a control point.
3. **Enforced** — bypass paths in the stated threat model were blocked.
4. **Attested** — enforcement evidence is bound to the exact Run and provider.

A strong guarantee in one dimension does not upgrade another dimension. A
staged workspace is not proof of network isolation, and captured traffic is not
proof that unobserved sockets were impossible.

The end-to-end chain is:

```text
requested capability
  → admission decision
  → installed mechanism
  → provider evidence
  → observed Effect
  → terminal result
  → configured event-carried history
```

This final event path is narrower than the Run Bundle: it does not currently
publish the complete Artifact, lineage, filesystem Effect,
AgentCtl/network/resource Evidence, output, or metrics inventory.

Read [Capabilities and evidence](capabilities-and-evidence.md)
for the user model, [pVisor isolation design](../design/isolation.md) and
[OverlayNet](../design/overlaynet.md) for mechanisms.
