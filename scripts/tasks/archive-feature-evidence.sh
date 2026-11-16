#!/usr/bin/env bash
set -euo pipefail
if [[ $# -ne 0 ]]; then echo "archive-feature-evidence takes no arguments" >&2; exit 2; fi
: "${RUNNER_TEMP:?set RUNNER_TEMP to the owned CI temporary directory}"
source="$RUNNER_TEMP/feature-evidence"
archive="$RUNNER_TEMP/feature-evidence.tar.gz"
[[ -d "$source" && ! -L "$source" ]] || { echo "feature evidence must be a real directory" >&2; exit 1; }
[[ ! -e "$archive" && ! -L "$archive" ]] || { echo "feature archive must be fresh" >&2; exit 1; }
temporary=$(mktemp "$RUNNER_TEMP/feature-evidence.XXXXXX")
trap 'rm -f -- "$temporary"' EXIT
inputs=(--directory "$RUNNER_TEMP" --add-file feature-evidence)
if [[ -n "${PVISOR_CASE_ARTIFACTS:-}" ]]; then
  [[ -d "$PVISOR_CASE_ARTIFACTS" && ! -L "$PVISOR_CASE_ARTIFACTS" ]] || {
    echo "scenario evidence must be a real directory" >&2; exit 1;
  }
  cases=$(cd "$PVISOR_CASE_ARTIFACTS" && pwd -P)
  [[ "$cases" != / && "${cases##*/}" != feature-evidence ]] || {
    echo "scenario evidence must have a distinct owned directory" >&2; exit 1;
  }
  inputs+=(--directory "${cases%/*}" --add-file "${cases##*/}")
fi
# OCI rootfs directories can deliberately deny traversal to the runner. Read
# them without changing evidence ownership/modes, following links or recursing
# into other filesystems. Redirection keeps the archive owned by the runner.
sudo -n tar --create --gzip --numeric-owner --one-file-system \
  --file - "${inputs[@]}" > "$temporary"
# Publish only a complete archive; never replace an existing result.
ln "$temporary" "$archive"
