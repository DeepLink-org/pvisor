#!/bin/sh
set -eu
if [ "$#" -ne 2 ]; then
    echo "usage: PVISOR_RECLAIM_LIB_DIR=GUEST_LIB_DIR sh build.sh aarch64|x86_64 OUTPUT_DIRECTORY" >&2
    exit 2
fi
case "$1" in aarch64) loader=/lib/ld-linux-aarch64.so.1 ;; x86_64) loader=/lib64/ld-linux-x86-64.so.2 ;; *) echo "unsupported architecture: $1" >&2; exit 2;; esac
: "${PVISOR_RECLAIM_LIB_DIR:?select the guest directory containing libc.so.6, libpthread.so.0 and libdl.so.2}"
source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
lib_dir=$(CDPATH= cd -- "$PVISOR_RECLAIM_LIB_DIR" && pwd)
target_dir="$source_dir/../../target/allocator-reclaim"
guest_arch=$1
for library in libc.so.6 libpthread.so.0 libdl.so.2; do
    test -f "$lib_dir/$library" || { echo "missing $lib_dir/$library" >&2; exit 2; }
done
build_gnu() {
    cargo rustc --locked --release --manifest-path "$source_dir/Cargo.toml" \
        --target "$guest_arch-unknown-linux-gnu" --target-dir "$target_dir" "$@" \
        -- -C linker=rust-lld -L "native=$lib_dir" -C link-arg=--dynamic-linker="$loader" -C link-arg=--no-allow-shlib-undefined
}
build_gnu --lib
build_gnu --example interposer
for binary in subject payload; do build_gnu --bin "$binary"; done
cargo rustc --locked --release --manifest-path "$source_dir/Cargo.toml" \
    --bin static-subject --target "$1-unknown-linux-musl" --target-dir "$target_dir" \
    -- -C linker=rust-lld
mkdir -p -- "$2"
cp "$target_dir/$1-unknown-linux-gnu/release/libpvisor_allocator_reclaim.so" "$2/libpvisor-reclaim.so"
cp "$target_dir/$1-unknown-linux-gnu/release/subject" "$target_dir/$1-unknown-linux-gnu/release/payload" "$2/"
cp "$target_dir/$1-unknown-linux-gnu/release/examples/libinterposer.so" "$2/interposer.so"
cp "$target_dir/$1-unknown-linux-musl/release/static-subject" "$2/"
