#!/usr/bin/env bash
# Private LAN with working DNS/NAT; no host network configuration changes.
set -euo pipefail
exec python3 "$(dirname "$0")/ubuntu_vm_network.py" "$@"
