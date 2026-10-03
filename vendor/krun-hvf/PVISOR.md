# pVisor HVF patch

Source: crates.io `krun-hvf` 0.1.0-1.19.3, package checksum
`2f7e78f0c5431195ca36aded1886024872699a6a56f0a600f619aeb7ef2161bc`.
Upstream: https://github.com/containers/libkrun ; license Apache-2.0,
with original source notices retained. Original manifest is `Cargo.toml.orig`.

Local changes:

- Link Hypervisor.framework in its owning wrapper crate, including direct examples.
- Add guest permission control and an optional VM-local RAM fault resolver.
- Route data/instruction RAM faults before MMIO decoding and PC advancement.
- Propagate resolver failures; default resolver declines every fault.

Real HVF check: `cargo build --locked -p pvisor --example hvf_ram_fault_case`,
then sign with the existing macOS entitlement and run retry/reject/error/fetch.
`tools/experiments/macos-memory/run.py` performs all four checks and saves evidence.
The VMM registers a resolver only after CPU pause/device drain; this patch alone
neither enables compression nor alters default RAM mapping permissions.
