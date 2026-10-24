#!/usr/bin/env bash
set -euo pipefail
if [[ $# -gt 1 ]]; then echo "too many arguments for wheel" >&2; exit 2; fi
if [[ $# -lt 1 ]]; then set -- "$@" release; fi
case "$1" in
  release) cargo_profile=release ;;
  debug) cargo_profile=dev ;;
  *) echo "expected debug or release, got: $1" >&2; exit 2 ;;
esac
mkdir -p "$CARGO_TARGET_DIR"
staging=$(mktemp -d "$CARGO_TARGET_DIR/wheel.XXXXXX")
trap 'rm -rf "$staging"' EXIT
if [[ "${PVISOR_WHEEL_PORTABLE:-0}" == 1 ]]; then
  export CIBW_CONFIG_SETTINGS="cargo-profile=$cargo_profile"
  uv run --locked --only-group dev cibuildwheel --output-dir "$staging"
else
  uv build --wheel --out-dir "$staging" --config-setting "cargo-profile=$cargo_profile"
fi
for wheel in "$staging"/pvisor-*.whl; do
  python3 scripts/packaging/verify_wheel.py "$wheel" --install-smoke
done
uv run --locked --only-group dev twine check "$staging"/pvisor-*.whl
mkdir -p dist
mv "$staging"/pvisor-*.whl dist/
for source in "$staging"/pvisor-firmware-source-*.tar.gz; do
  if [[ -f "$source" ]]; then mv "$source" dist/; fi
done
