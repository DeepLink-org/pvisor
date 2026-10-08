#!/usr/bin/env bash
set -euo pipefail
example_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$example_dir/../../.."
exec uv run --extra dev pytest -q tests/test_zcode_integration.py --zcode-integration
