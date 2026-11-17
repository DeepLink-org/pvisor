#!/usr/bin/env bash
set -euo pipefail
if [[ $# -lt 1 || $# -gt 2 || ( $# -eq 2 && "$2" != --hardware ) ]]; then
  echo "expected docker-cli or docker-compose [--hardware]" >&2
  exit 2
fi
# Default and ignored hardware tests use the same Docker/Compose ownership partition.
compose='test(compose)'
case "$1" in
  docker-cli)
    targets=(--test docker_cli --test docker_p0_cli --test docker_staged_cli --test docker_staged_native)
    filter="not ($compose)" ;;
  docker-compose)
    targets=(--test compose_cli --test docker_cli)
    filter="binary(=compose_cli) | $compose" ;;
  *) echo "unknown native feature: $1" >&2; exit 2 ;;
esac
extra=()
if [[ "${2:-}" == --hardware ]]; then
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) [[ -r /dev/kvm && -w /dev/kvm ]] ;;
    Darwin-arm64) [[ "$(sysctl -n kern.hv_support)" == 1 ]] ;;
    *) echo "native hardware requires Linux/KVM or Apple Silicon/HVF" >&2; exit 1 ;;
  esac
  [[ -n "${PVISOR_DOCKER_TEST_ROOTFS:-}${PVISOR_DOCKER_TEST_IMAGE:-}" ]] || {
    echo "native hardware requires PVISOR_DOCKER_TEST_ROOTFS or PVISOR_DOCKER_TEST_IMAGE" >&2
    exit 1
  }
  extra=(--run-ignored only --test-threads 2)
fi
# Bash 3.2 treats an empty array as unset under nounset. Keep strict mode and
# forward optional hardware arguments only when the array has elements.
mise run test-rust -- pvisor-cli -- "${targets[@]}" -E "$filter" --no-tests fail ${extra[@]+"${extra[@]}"}
