# Experimental OCI preparation helpers

Question: can OCI preparation avoid unused bundle output and a second metadata
pass while preserving the audited rootfs? This is [B-OCI-PREP-ENG](../../REGISTRY.md#b-oci-prep-eng),
an engineering experiment, not a product startup benchmark.

## Scope

The patch targets **umoci v0.4.7**, Apache-2.0, and adds the opt-in
`raw unpack --rootless --pvisor-metadata` flag. Without it, upstream behavior is
unchanged. It does not change the installed umoci or pVisor product code.
`raw unpack` omits the bundle/config/mtree output that this experiment does not use.

The experimental profile requires a new rootfs and one uncompressed OCI layer.
It rejects whiteouts, special nodes, reserved identity xattrs, non-user xattrs,
non-rootless execution and `--keep-dirlinks`. Symlink ownership must be achievable
by the caller. The extracted tree is private; concurrent mutation is unsupported.
This is not a general-purpose OCI importer or an upstream release.

Guest UID/GID/mode and archived xattrs are applied before rootless header remapping.
Host access permissions remain separate. Hardlinks share their inode; symlink
ownership uses no-follow operations. Existing parent-directory restoration is
retained. No parent-directory batching or hash optimization is included.

## Build and qualify

Linux, Python 3.12+, `patch`, and Go are required (validated with Go 1.24.3).
Download the pinned source archive yourself; the builder verifies its SHA-256,
uses vendored dependencies offline, and refuses an existing output directory:

```sh
curl -fL https://codeload.github.com/opencontainers/umoci/tar.gz/refs/tags/v0.4.7 -o /tmp/umoci-v0.4.7.tar.gz
python3 benchmark/startup/oci_prepare/build.py \
  --archive /tmp/umoci-v0.4.7.tar.gz \
  --output /tmp/umoci-pvisor-build --go /absolute/path/to/go
python3 benchmark/startup/oci_prepare/qualify.py \
  --binary /tmp/umoci-pvisor-build/umoci-experiment \
  --reference /absolute/path/to/original/umoci
```

The builder runs focused Go metadata and original path-security tests. Qualification
uses only small temporary OCI fixtures, checks default/reference equivalence,
compares both restored trees, tests unsupported inputs, and rejects corrupted
payloads. It does not install services, launch VMs, run SWE or change caches.
Build/test resource limits are supplied by the caller; these helpers do not grant
permission to launch a performance batch.

For a separately authorized preparation run, the baseline is `raw unpack` followed
by `restore_metadata.py`; the candidate uses `--pvisor-metadata` without that second
pass. Both must pass `audit_image.py LAYER.tar ROOTFS`. The audit checks every
payload, Guest identity, archived xattr, hardlink and symlink owner. It is not a
complete arbitrary-OCI validator, and does not prove directory timestamp equality.
Never replace the audit with elapsed-time success. Keep errors and partial output;
these helpers do not retry or clean failed performance runs.

## Recorded preparation observations

GPU03, one sequential sample per arm on 2026-10-29; independent inode copies of
the same OCI, target-only eviction and zero-residency checks, no global cache drop.
Condition 31.75 GiB / 5975% CPU plus coordinator 256 MiB / 25%, zero swap;
preparation 900 s, helper 2800 s, batch 4200 s, controller 5400 s.
Source and inactive caches are outside the active condition budget.

| Same experimental binary | Two-pass raw | Single-pass raw |
| --- | ---: | ---: |
| Extraction, seconds (candidate includes metadata) | 28.408182 | 30.309816 |
| Separate metadata pass, seconds | 10.255335 | Integrated |
| Total preparation, seconds | 38.663516 | 30.309831 |
| Full audit outside preparation, seconds | 18.429036 | 19.411061 |

Both passed the unchanged audit: 133,830 files, 26,048 directories and 2,113 links.
All workers exited, no OOM/swap, maximum sampling gap 0.512 s. These are exploratory
observations, not a randomized speedup estimate or a confidence interval. No VM or
SWE was run. Full local evidence is not distributed with the checkout.

Upstream archive SHA-256:
`c01b36de6fdc513eb65add57bc882d72f94fc3b4b65a8f9ef59826fb754af93e`.
Measured experiment binary SHA-256:
`aaf4675a8d0f6b6c60118f38919697d077bbcdf6b4b2c35b36dd0e8c41344fa1`.
The original measurements used the frozen experiment harness; the portable build
and qualification packaging does not introduce new performance samples.
