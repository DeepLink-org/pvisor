# Shared benchmark support

These modules support the active JSON experiments listed in
[registry.csv](../registry.csv); requirements and publication rules live in
[REGISTRY.md](../REGISTRY.md). They retain input/provenance validation, bounded
namespace isolation, framed counting proxies, payload byte oracles and resource
observers. Historical campaign loops and publication entry points are retired.
Keep correctness checks and failure retention intact when changing these helpers.

`startup/common.py` and the memory coordinators add this directory explicitly to
their import path. Image adapters copy these helper sources into each batch's
frozen harness. A retained historical batch continues to use its own frozen
sources; do not rewrite its paths, manifests or hashes to match current modules.

Prepare a stock/custom Firecracker kernel receipt without starting a VM:

```sh
python3 benchmark.new/support/firecracker_kernels.py --help
```

Provide exact kernel/config/provenance bytes and a trusted extractor for stock
vmlinuz. Preparation retains their hashes and rejects an existing output. It
neither downloads kernels nor substitutes pVisor firmware for stock kernels.

Prepare a new private rootless Docker budget before filesystem experiments:

```sh
python3 benchmark.new/support/prepare_reference_budget.py \
  --output benchmark.new/.data/NEW_PRIVATE_DOCKER_BUDGET \
  --memory-mib 2048 --cpu-affinity 0,1 --cpu-placement affinity
```

This creates only fresh private units and stores; it requires a delegated user
cgroup manager and installed rootless Docker tools. It never changes global
settings or adopts an existing daemon. Pass the resulting `setup.json` to the
owning experiment and retain it for scoped cleanup. Affinity is not a cpuset
placement guarantee. Kernel and runtime capability checks remain separate.

Run the helper contracts with `mise run test-py -- discover -s benchmark.new/support`.
The default `mise run test-py` includes these contracts and all active benchmark suites.
