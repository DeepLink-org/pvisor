# Cold OCI to a running parent and four live children

The first VM can use the extracted filesystem while the object pool publishes the reusable image. On Linux/KVM this moved the first verified command from 142.033 s to 20.770 s. The reusable image became available at 106.001 s. This directory records an engineering prototype based on commit `98f58516a2e34c0f161aa3ff5f19e30c8b38f2d4`.

## End-to-end measurements

Each cell is a median of three trials, in seconds. All timestamps come from the same monotonic clock within a trial. The fork interval starts after the parent writes its marker. The final column measures the complete interval, including that write. [Per-trial measurements](end_to_end.csv).

| Execution path | Cold OCI to parent command | Fork request to four verified children | Cold OCI to four verified children |
| --- | ---: | ---: | ---: |
| AgentENV cold create | 24.064 | 0.529 | 24.639 |
| pVisor, per-object directory sync, wait for pull | 142.033 | 0.605 | 142.697 |
| pVisor, batched directory sync, start from extracted lower | 20.770 | 0.492 | 21.331 |

## Prototype changes

Object-pool publication accounted for 120 s of a 144 s cold pull. The prototype batches directory syncs after the object writers finish, retaining file sync and digest verification. This reduced median time to first command from 143 s to 108 s in the sync experiment.

With batching enabled, the first VM can mount the verified extracted rootfs with a private upper while image publication continues. In the separate early-start experiment this reduced time to first command from 109 s to 21 s. [Original phase and A/B measurements](https://github.com/DeepLink-org/pvisor/tree/ea0116182b2e1d81c743c143592e3ca3ff8d2e90/benchmark/startup/cold_oci_fork).

## Protocol and correctness

- Intel Xeon Gold 6526Y, Linux 6.17.0-19-generic x86-64, ext4, KVM. Each VM had 2 vCPUs and 7 GiB. The Vuls OCI manifest was `sha256:8ef54cd690e477805c99bf6355d0ca41d41d9fb70a4d78ff3fd8cf3c1a50631e`, with one 3.60 GiB uncompressed tar layer.
- Each trial hashed an independent copy of the inputs, evicted only the copy's pages and verified zero resident pages. Timing began before registry/service startup and included image processing through the identical guest Python response (`ready=true, cpus=2`). Fixture staging, certificate generation and WAN download were outside the clock. pVisor used local HTTPS; AgentENV used local HTTP and a private network namespace.
- Each parent wrote a private marker, then forked four children. The end timestamp was the last successful concurrent check of marker inheritance and CPU count. A subsequent write in one child stayed isolated from its siblings and the parent. In every optimized trial, all four children were verified before durable publication. A fresh VM then reopened the published image without the private marker.
- The harness holds a lease on staging for the lifetime of all dependent VMs and verifies the lower's path and inode in the experimental fork path. These tests cover normal exits. Production adoption needs lifecycle ownership independent of the harness, publisher-failure handling, cancellation and crash recovery tests.

## Reproducing the pVisor end-to-end experiment

Use a separate worktree at the measured revision. The [prototype patch](measured.patch) includes the timing probes, private early catalog and fork support. The patched executable is for an isolated benchmark account and private scratch directory.

```sh
experiment=$PWD/benchmark/startup/cold_oci_fork
git worktree add --detach ../pvisor-cold-experiment 98f58516a2e34c0f161aa3ff5f19e30c8b38f2d4
git -C ../pvisor-cold-experiment apply "$experiment/measured.patch"
(cd ../pvisor-cold-experiment && cargo build --locked --release -p pvisor-cli)
```

On a Linux KVM host with Python 3.11+, OpenSSL and the matching libkrunfw.so.5 beside the binary, run:

```sh
BENCH_PVISOR=/absolute/path/to/pvisor \
BENCH_SCRATCH=/absolute/path/to/private-scratch \
BENCH_OCI_FIXTURE=/absolute/path/to/vuls-registry-fixture \
python3 "$experiment/run.py" > results.jsonl
```

Provide the local fixture separately. Its layout is `blobs/sha256/<digest>` plus `registry.json` with a `vuls-666` record containing standard OCI `manifest`, `config` and `layer` descriptors (digest, mediaType, size). The runner alternates early/serial, serial/early and early/serial, checks every blob, closes all VMs and removes trial-specific stores. The recorded AgentENV server SHA-256 was `0d8980af726e38a25612a4b3a13021eae76832710d4bda5ad063f1409684c1ca`; its independent run used cold create followed by the count-four fork API.
