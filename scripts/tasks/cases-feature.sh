#!/usr/bin/env bash
set -euo pipefail
if [[ $# -eq 0 || "$1" == -* ]]; then
  echo "expected FEATURE followed by cases arguments" >&2
  exit 2
fi
feature="$1"
shift
# Keep one feature owner even when callers forward native case selections.
for arg in "$@"; do
  case "$arg" in
    --feature|--feature=*|--suite|--suite=*)
      echo "feature task does not accept another feature or historical suite" >&2
      exit 2 ;;
  esac
done
exec bash "$MISE_PROJECT_ROOT/scripts/tasks/cases.sh" --feature "$feature" "$@"
