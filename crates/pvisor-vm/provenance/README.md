# Imported runtime provenance

The private `src/{arch,arch_gen,cpuid,devices,hvf,kernel,polly,smbios,utils,vmm}`
modules consolidate libkrun 1.19.3 components into `pvisor-vm`. Upstream copyright
and SPDX headers remain in the imported source; the repository Apache-2.0 license
applies to those components. This directory preserves original Cargo manifests,
packaged VCS records and the existing selective-backport ledger.

`libkrun/UPSTREAM.md` describes the import baseline and pre-merge backports; its
old paths are historical coordinates. It does not describe the current API or
claim that every optional upstream feature has been validated. Firmware v5 data
loading, Hypervisor FFI and its minimal SIMD shim are private runtime boundaries.

Source: https://github.com/containers/libkrun

Current first-party adapters: `api.rs`, `backend.rs`, `builder.rs`, `firmware.rs`,
`handle.rs`, `memory.rs`, `portable.rs`, and the owned Rust guest build integration.
Only `pvisor_vm::api` is public. Core runtime components compile inside this
crate; optional GPU/input libraries are private optional dependencies.
