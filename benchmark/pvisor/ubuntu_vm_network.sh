#!/usr/bin/env bash
# Private network namespace and TAP for the full Ubuntu guest; no host routing changes.
set -euo pipefail
exec unshare --user --map-root-user --net -- /bin/sh -c '
  set -eu
  ip link set lo up
  ip tuntap add pvbench-tap mode tap
  ip addr add 10.77.0.1/24 dev pvbench-tap
  ip link set pvbench-tap up
  exec "$@"
' ubuntu-reference "$@"
