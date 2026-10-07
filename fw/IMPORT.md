# Firmware source import

Imported into `pvisor/fw` on 2026-10-07 from the current libkrunfw working tree.
The original checkout was retained for verification; it is not a build input
required by this directory.

- Upstream: <https://github.com/libkrun/libkrunfw>
- pVisor fork: <https://github.com/reiase/libkrunfw>
- Fork branch at import: `pvisor/aarch64-minimal-config`
- Fork HEAD: `b3c0abf89bcecb0f6e664d5a1a31b6e6af567742`
- Firmware version: `5.6.2`; ABI version: `5`
- Linux version: `6.12.109`

## Included pVisor changes

The imported HEAD already contains the aarch64 configuration trimming,
x86-64 guest feature trimming, compact x86-64 bundle generation and compiled
ABI roundtrip regression tests. The import also includes these uncommitted
working-tree changes, rather than merely exporting HEAD:

- `Makefile`: build generic Linux x86-64 `vmlinux` only, skipping `bzImage`.
- `config-libkrunfw_x86_64`: disable IPv6 for the IPv4-only guest.
- `patches/0040-tsi-guard-ipv6-udp-lookup.patch`: guard the IPv6 UDP lookup
  with `IS_REACHABLE(CONFIG_IPV6)` so TSI can build without IPv6.
- `README.md`: document IPv4-only behavior and the `vmlinux` build path.

All 71 imported files were byte-compared with the original working tree before
adding pVisor integration documentation. No source optimization was dropped.
The x86-64 configuration also disables guest CPU mitigations; see `README.md`
for the security and capability tradeoffs. An import is not new performance or
end-to-end VM validation.

## Import boundary

Included: Makefile, bundle generator, guest configs, all patches, host headers,
build scripts, tests, utility sources, SEV/TDX boot assets and license files.
Excluded: `.git`, standalone `.github` workflows and CODEOWNERS, local agent or
credential directories, downloaded/extracted Linux sources, Python caches,
generated `kernel.c` and built shared libraries.

Linux and kernel patches retain GPL-2.0-only licensing; library/generator code
retains LGPL-2.1-only licensing. The macOS compatibility headers retain their
own license in `include/macos-host/LICENSE`. Do not treat `fw/` as Apache-2.0
first-party code or distribute firmware without its corresponding source and
license materials.
