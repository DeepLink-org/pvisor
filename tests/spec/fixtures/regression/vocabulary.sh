#!/usr/bin/env bash
set -Eeuo pipefail
if [[ $# -gt 0 ]]; then echo "too many arguments for test-vocabulary" >&2; exit 2; fi
work=$(mktemp -d)
trap 'echo "FAIL: $BASH_COMMAND" >&2; cat "$work/stdout" "$work/stderr" >&2 2>/dev/null || true' ERR
trap 'rm -rf "$work"' EXIT
cp "$MISE_PROJECT_ROOT/tests/spec/fixtures/doc.bash" "$work/reference.bash"
cp "$MISE_PROJECT_ROOT/tests/spec/fixtures/vm.bash" "$work/vm.bash"
export CASE_FIXTURES="$MISE_PROJECT_ROOT/tests/spec/fixtures"
export CASE_ROOT="$work/case with spaces" WS="$work/case with spaces/ws" SUBJECT_BIN=/usr/bin/true
mkdir -p "$WS"
cd "$WS"
run_vocab() {
  got=0
  bash -euo pipefail -c 'source "$1"; source /dev/stdin' vocab-test "$work/reference.bash" "$work/vm.bash" > "$work/stdout" 2> "$work/stderr" || got=$?
  test "$got" -eq "$1"
}
run_vocab 0 <<'SH'
case_setup
case_run nonzero <<'COMMAND'
printf before; false; printf leaked > leaked
COMMAND
SH
test ! -e "$WS/leaked"
test "$(cat "$CASE_ROOT/command.log")" == before
run_vocab 1 <<'SH'
case_setup
case_run success <<'COMMAND'
false; printf leaked > leaked
COMMAND
SH
test ! -e "$WS/leaked"
run_vocab 1 <<'SH'
case_setup
case_run nonzero <<'COMMAND'
true
COMMAND
SH
export SUBJECT_BIN="$work/subject with spaces"
printf '#!/bin/sh\nprintf "chosen subject\\n"\n' > "$SUBJECT_BIN"
chmod +x "$SUBJECT_BIN"
run_vocab 0 <<'SH'
case_setup
case_run success <<'COMMAND'
pvisor
COMMAND
stdout_has 'chosen subject'
mkdir -p "$PVISOR_CASE_RECORDS/session"
printf '%s' '{"run":{"state":"completed","exit_code":0},"safety":{"enabled":true},"changes":[{"path":"note"}]}' > "$PVISOR_CASE_RECORDS/session/run-bundle.json"
printf '%s' '{"overlay":null}' > "$PVISOR_CASE_RECORDS/session/run.json"
bundle_expect run.state completed
bundle_expect run.exit_code 0
bundle_expect safety.enabled true
bundle_expect changes.0.path note
bundle_contains changes note
record_expect overlay null
SH
diff -u <(printf 'chosen subject\n') "$CASE_ROOT/command.log"
jq -e '.invocation.program | startswith("/")' "$WS/run-spec.json" >/dev/null
read -r proxy gateway < "$CASE_ROOT/ports"
test "$proxy" != "$gateway"

got=0
PVISOR_CASE_ROOTFS="$work/missing" PVISOR_CASE_VM_DRIVER="$work/missing-driver" bash -euo pipefail -c 'source "$1"; source "$2"; require_vm_sdk; echo reached' vocab-test "$work/reference.bash" "$work/vm.bash" > "$work/stdout" 2> "$work/stderr" || got=$?
test "$got" -eq 77
grep -Fq SKIP: "$work/stderr"
! grep -Fq reached "$work/stdout"
got=0
PVISOR_CASE_AGENT='' bash -euo pipefail -c 'source "$1"; require_agent; echo reached' vocab-test "$work/reference.bash" "$work/vm.bash" > "$work/stdout" 2> "$work/stderr" || got=$?
test "$got" -eq 77
grep -Fq SKIP: "$work/stderr"
! grep -Fq reached "$work/stdout"
export VM_CASE_DRIVER="$work/driver"
printf '#!/bin/sh\nprintf "sdk-failure-detail\\n"\nexit 23\n' > "$VM_CASE_DRIVER"
chmod +x "$VM_CASE_DRIVER"
got=0
bash -euo pipefail -c 'source "$1"; source "$2"; vm_run_sdk compressed' vocab-test "$work/reference.bash" "$work/vm.bash" > "$work/stdout" 2> "$work/stderr" || got=$?
test "$got" -ne 0
grep -Fq sdk-failure-detail "$work/stdout"
grep -Fq exit=23 "$work/stderr"

cp "$MISE_PROJECT_ROOT/tests/spec/fixtures/stage.bash" "$work/stage.bash"
mkdir "$work/bin"
cp "$SUBJECT_BIN" "$work/bin/pvisor"
bash -euo pipefail -c 'unset SUBJECT_BIN; export PATH="$2:$PATH"; source "$1"; pvisor' vocab-test "$work/stage.bash" "$work/bin" > "$work/stdout"
test "$(cat "$work/stdout")" == 'chosen subject'

cp "$MISE_PROJECT_ROOT/tests/spec/fixtures/use.bash" "$work/journey.bash"
export DOCUMENT="$work/value.json"
printf '%s' '{"flag":true,"changes":[]}' > "$DOCUMENT"
for assertion in 'json_expect "$DOCUMENT" /flag 1' 'json_expect "$DOCUMENT" /missing null' 'json_paths "$DOCUMENT"'; do
  got=0
  bash -euo pipefail -c 'source "$1"; journey_setup; source /dev/stdin' vocab-test "$work/journey.bash" <<< "$assertion" > "$work/stdout" 2> "$work/stderr" || got=$?
  test "$got" -ne 0
done
bash -euo pipefail -c 'source "$1"; journey_setup; printf "%s" "$HOME"; json_expect "$DOCUMENT" /flag true' vocab-test "$work/journey.bash" > "$work/stdout" 2> "$work/stderr"
test "$(cat "$work/stdout")" == "$CASE_ROOT/home"
echo 'PASS: vocabulary errexit, subject paths, JSON assertions, prerequisites and SDK diagnostics'
