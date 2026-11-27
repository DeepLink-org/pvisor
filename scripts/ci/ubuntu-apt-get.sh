#!/usr/bin/env bash
set -euo pipefail

# Keep the runner's Ubuntu mirrors and signing keys, not unrelated vendor feeds.
sources=/etc/apt/sources.list.d/ubuntu.sources
if [[ ! -s $sources ]]; then
  sources=/etc/apt/sources.list
fi
[[ -s $sources ]] || { echo 'Ubuntu archive sources are missing' >&2; exit 1; }
exec sudo apt-get -o "Dir::Etc::sourcelist=$sources" -o Dir::Etc::sourceparts=- "$@"
