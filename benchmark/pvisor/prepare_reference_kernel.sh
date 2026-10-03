#!/usr/bin/env bash
# Linux 6.12.109 source directory and a fresh output directory; no host changes.
set -euo pipefail
source_dir=$(realpath "$1")
output_dir=$(realpath -m "$2")
script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
test ! -e "$output_dir"
mkdir -p "$output_dir"
cp "$script_dir/reference_kernel.config" "$output_dir/.config"
make -C "$source_dir" O="$output_dir" olddefconfig
make -C "$source_dir" O="$output_dir" -j"${REFERENCE_BUILD_JOBS:-8}" bzImage vmlinux
