#!/usr/bin/env bash
set -euo pipefail
if [[ $# -eq 0 ]]; then
  set -- "fmt-rust --check" "fmt-py --check" lint test build
fi
for command in "$@"; do
  read -r -a words <<< "$command"
  if [[ ${#words[@]} -eq 0 ]]; then echo "empty CI command" >&2; exit 2; fi
  if [[ "${words[0]}" == -* ]]; then echo "invalid CI task: ${words[0]}" >&2; exit 2; fi
  mise run "${words[0]}" -- "${words[@]:1}"
done
