#!/usr/bin/env bash
set -Eeuo pipefail
if [[ $# != 0 ]]; then echo "too many arguments" >&2; exit 2; fi
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/runner"
export APT_TEST_ROOT="$work" RUNNER_TEMP="$work/runner"
cat > "$work/bin/sudo" <<'SH'
#!/usr/bin/env bash
exec "$@"
SH
cat > "$work/bin/timeout" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == --kill-after=10s ]]
[[ "$2" == 180s || "$2" == 360s ]]
printf '%s\n' "$2" >> "$APT_TEST_ROOT/timeouts"
shift 2
exec "$@"
SH
cat > "$work/bin/apt-get" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
source_list= lists=
while [[ "$1" == -o ]]; do
  case "$2" in
    Dir::Etc::sourcelist=*) source_list=${2#*=} ;;
    Dir::State::lists=*) lists=${2#*=} ;;
    Dir::Etc::sourceparts=-|Acquire::Retries=2|Acquire::http::Timeout=20|Acquire::https::Timeout=20|APT::Update::Error-Mode=any|Dpkg::Use-Pty=0) ;;
    *) echo "unexpected apt configuration $2" >&2; exit 99 ;;
  esac
  shift 2
done
[[ -d "$lists/partial" && -f "$source_list" ]]
[[ "$lists" == "$RUNNER_TEMP"/pvisor-windows-apt.*/lists ]]
[[ "$DEBIAN_FRONTEND" == noninteractive ]]
grep -Fq 'signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg' "$source_list"
grep -Fq 'https://archive.ubuntu.com/ubuntu noble-updates' "$source_list"
grep -Fq 'https://security.ubuntu.com/ubuntu noble-security' "$source_list"
! grep -Eq 'azure|trusted=yes|allow-insecure' "$source_list"
printf '%s\n' "$1" >> "$APT_TEST_ROOT/operations"
if [[ "$1" == "$APT_TEST_FAIL" ]]; then exit 42; fi
if [[ "$1" == install ]]; then
  found=0 filesystem=0
  for package in "$@"; do
    [[ "$package" != gcc-mingw-w64-x86-64 ]]
    if [[ "$package" == gcc-mingw-w64-x86-64-posix ]]; then found=1; fi
    if [[ "$package" == e2fsprogs ]]; then filesystem=1; fi
  done
  [[ "$found" == 1 && "$filesystem" == 1 ]]
fi
SH
chmod +x "$work/bin/"*
export PATH="$work/bin:$PATH"
for failure in none update install; do
  : > "$work/operations"
  : > "$work/timeouts"
  got=0
  APT_TEST_FAIL="$failure" bash "$MISE_PROJECT_ROOT/scripts/ci/install-windows-fw-deps.sh" noble || got=$?
  if [[ "$failure" == none ]]; then
    [[ "$got" == 0 ]]
  else
    [[ "$got" == 42 ]]
  fi
  if [[ "$failure" == update ]]; then
    [[ "$(cat "$work/operations")" == update ]]
    [[ "$(cat "$work/timeouts")" == 180s ]]
  else
    [[ "$(cat "$work/operations")" == $'update\ninstall' ]]
    [[ "$(cat "$work/timeouts")" == $'180s\n360s' ]]
  fi
  [[ -z "$(find "$work/runner" -mindepth 1 -print -quit)" ]]
done
echo 'PASS: isolated signed mirrors, bounded setup, minimal compiler, failure propagation and cleanup'
