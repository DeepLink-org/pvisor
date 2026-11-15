# Shared benchmark support

The active [registry](../registry.csv) entries run native Rust/Criterion and do
not import or execute these Python modules. Publication requirements remain in
[REGISTRY.md](../REGISTRY.md).

Retained callers are separate from that measurement chain:

- `startup/audit.py` reads `reference_inputs.py` and `firecracker_kernels.py`
  to verify saved input and kernel identities.
- `lazy_startup.py` and `lazy_image_v2.py` supply independent image/protocol
  fixtures and their correctness tests.
- `resource_budget.py` and `prepare_reference_budget.py` support private,
  explicitly owned resource-budget diagnostics, not Criterion sampling.
- `memory_scale.py`, `memory_savings.py`, `firecracker_ksm.py` and
  `linux_cold_runtime.py` retain SDK diagnostic/oracle contracts and their tests.
  The active allocator, scanner, pool and library experiments use native workers
  and native preparation instead.

Keep correctness checks and failure retention intact. A retained batch uses its
own frozen sources; never rewrite its paths, manifests or hashes to match the
current checkout.

Prepare a stock/custom Firecracker kernel receipt without starting a VM:

```sh
python3 benchmark/support/firecracker_kernels.py --help
```

Provide exact kernel/config/provenance bytes and a trusted extractor for stock
vmlinuz. Preparation retains their hashes and rejects an existing output. It
neither downloads kernels nor substitutes pVisor firmware for stock kernels.

Prepare a new private rootless Docker budget before filesystem experiments:

```sh
python3 benchmark/support/prepare_reference_budget.py \
  --output benchmark/.data/NEW_PRIVATE_DOCKER_BUDGET \
  --memory-mib 2048 --cpu-affinity 0,1 --cpu-placement affinity
```

This creates only fresh private units and stores; it requires a delegated user
cgroup manager and installed rootless Docker tools. It never changes global
settings or adopts an existing daemon. Pass the resulting `setup.json` to the
owning experiment and retain it for scoped cleanup. Affinity is not a cpuset
placement guarantee. Kernel and runtime capability checks remain separate.

Run the helper contracts with `mise run test-py -- discover -s benchmark/support`.
The default Python test task includes these fixture and diagnostic contracts.
