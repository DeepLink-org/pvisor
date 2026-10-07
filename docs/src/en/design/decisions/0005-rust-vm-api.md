# 0005: Keep VM backend differences inside one Rust runtime {#adr-0005}

**Status.** Implementation backfill; this records the implemented boundary, not independent maintainer approval.

**Context.** VM configuration, device customizations and snapshots were split between pVisor and multiple libkrun crates. C contexts, borrowed raw pointers and public backend structs made ownership and platform differences leak into consumers.

**Options.** Retain an external C context ABI and vendored component crates, or consolidate private runtime modules behind one Rust contract.

**Current choice.** `pvisor-vm` compiles the core modules together. Only `api` is public; portable structs and traits define every public method signature without conditional compilation or default implementations. Private adapters implement those traits. Architecture layout and hypervisor behavior have separate internal traits; callers receive explicit unsupported errors and inspect capabilities. Firmware data ABI and OS FFI remain private.

**Migration and consequences.** Executor, shim, snapshot/checkpoint/pager, examples and the init benchmark use the new API. Hardware/device tests move inside the crate. Static kernel packing belongs to the runtime. Old core crate dependencies and VM control C functions are removed; existing evidence identifiers and serialized snapshot fields remain compatible. Uniform API shape does not promise cross-architecture restore. `api_contract` and `repository_boundary` guard the boundary; native tests and target compile checks supply distinct evidence. See [development](../../community/development.md) and the [runtime contract](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor-vm/README.md).
