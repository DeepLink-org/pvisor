#!/usr/bin/env bash
# Ubuntu 24.04 hosted-runner prerequisites; never run on a developer machine.
set -euo pipefail
if [[ $# != 1 || "$1" != noble ]]; then
  echo "usage: install-windows-fw-deps.sh noble (Ubuntu 24.04 CI only)" >&2
  exit 2
fi

work=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/pvisor-windows-apt.XXXXXX")
trap 'sudo rm -rf -- "$work"' EXIT
# apt's unprivileged download user must be able to traverse the scratch tree.
chmod 755 "$work"
mkdir -p "$work/lists/partial"
cat > "$work/sources.list" <<'SOURCES'
deb [signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://archive.ubuntu.com/ubuntu noble main universe
deb [signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://archive.ubuntu.com/ubuntu noble-updates main universe
deb [signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://security.ubuntu.com/ubuntu noble-security main universe
SOURCES
# Isolate lists as well as sources: stale hosted-runner indexes must not route
# package downloads back to the slow Azure mirror. Do not modify system sources.
apt_options=(
  -o "Dir::Etc::sourcelist=$work/sources.list"
  -o Dir::Etc::sourceparts=-
  -o "Dir::State::lists=$work/lists"
  -o Acquire::Retries=2
  -o Acquire::http::Timeout=20
  -o Acquire::https::Timeout=20
  -o APT::Update::Error-Mode=any
  -o Dpkg::Use-Pty=0
)
sudo timeout --kill-after=10s 180s env DEBIAN_FRONTEND=noninteractive \
  apt-get "${apt_options[@]}" update
# The meta-package installs both threading variants. The DLL needs only one
# compiler, and this variant supplies x86_64-w64-mingw32-gcc via alternatives.
sudo timeout --kill-after=10s 360s env DEBIAN_FRONTEND=noninteractive \
  apt-get "${apt_options[@]}" install -y --no-install-recommends \
  build-essential flex bison libelf-dev libssl-dev bc curl python3-pyelftools \
  gcc-mingw-w64-x86-64-posix
