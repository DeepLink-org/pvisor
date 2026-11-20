# Changelog

## 0.4.0

- Unix container commands use the Session lifecycle. Experimental daemonless
  `pvisor compose` starts and manages multi-service projects.
- `pvisor push` publishes lazy images to S3 or an absolute local directory.
  `pvisor image pull --from` imports a pinned source. OCI pulls accept credentials
  for a registry's Bearer token endpoint.
- DSH provides a task monitor. Eligible running Linux Host Sessions support
  terminal attachment with `pvisor exec --session`. Agent adapters prefer VM
  execution in `auto` mode.
- Eligible Linux/KVM containers support same-host CoW branches and explicit
  offload/resume for enrolled children. The Linux pressure service offloads
  enrolled VMs at cooperative wait boundaries.
- The containerd shim supports node-wide VM network defaults and optional
  guest TCP exposure inside the pod network namespace.
- Linux x86_64 and macOS ARM64 have stable archives and wheels. Windows x86_64
  has an experimental source-built WHP container profile and native CI.
- Apply reports failure and recovery evidence. Workspace snapshots exclude
  leased sibling stages, and replay collects resource samples separately from
  trajectory scheduling.
- Rust consumers use the owning crate's `api` where declared. Contributors can
  validate behavior with feature-scoped Scrut cases and native regression runners.

See the [0.4.0 release notes](docs/src/en/community/changelog.md#v040).
