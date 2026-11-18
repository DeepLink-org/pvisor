# Resource setup; product assertions stay in the Scrut cases.
test -x "${SUBJECT_BIN:-}" || { echo 'missing executable SUBJECT_BIN' >&2; exit 77; }
while IFS= read -r key; do
  case "$key" in
    AWS_*|PVISOR_CACHE_*|PVISOR_REGISTRY_*|DOCKER_CONFIG|REGISTRY_AUTH_FILE|SSL_CERT_FILE|SSL_CERT_DIR|HTTP_PROXY|HTTPS_PROXY|ALL_PROXY|http_proxy|https_proxy|all_proxy)
      unset "$key" ;;
  esac
done < <(compgen -e)
export NO_PROXY=127.0.0.1 no_proxy=127.0.0.1
export PVISOR_DAEMON_HOME="$CASE_ROOT/daemon"
LAZY_DAEMON_BIN="$(dirname "$SUBJECT_BIN")/pvisor-daemon"
cp "${BASH_SOURCE[0]}" "$CASE_ROOT/lazy-fixtures.bash"

lazy_require_daemon() {
  test -x "$LAZY_DAEMON_BIN" || { echo 'missing sibling pvisor-daemon' >&2; exit 77; }
}

lazy_wait_ready() {
  local file=$1 pid=$2
  for ((attempt=0; attempt<400; attempt++)); do
    if [ -s "$file" ] && python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$file" 2>/dev/null; then
      return 0
    fi
    kill -0 "$pid" 2>/dev/null || { echo "fixture exited before readiness: $file" >&2; exit 77; }
    sleep 0.05
  done
  echo "fixture readiness deadline: $file" >&2
  exit 77
}

lazy_start_registry() {
  command -v openssl >/dev/null || { echo 'OpenSSL is required' >&2; exit 77; }
  local architecture=${1:-}
  if [ -z "$architecture" ]; then
    case "$(uname -m)" in
      arm64|aarch64) architecture=arm64 ;;
      x86_64) architecture=amd64 ;;
      *) echo 'fixture requires amd64 or arm64' >&2; exit 77 ;;
    esac
  fi
  LAZY_PLATFORM="linux/$architecture"
  cp "$PVISOR_PROJECT_ROOT/crates/pvisor-cli/tests/common/registry.py" "$CASE_ROOT/registry.py"
  python3 "$CASE_ROOT/registry.py" "$CASE_ROOT/registry" "$architecture" \
    > "$CASE_ROOT/registry-ready.json" 2> "$CASE_ROOT/registry.stderr" &
  LAZY_REGISTRY_PID=$!
  lazy_wait_ready "$CASE_ROOT/registry-ready.json" "$LAZY_REGISTRY_PID"
  LAZY_OCI=$(journey_json "$CASE_ROOT/registry-ready.json" /reference)
  LAZY_DIGEST=$(journey_json "$CASE_ROOT/registry-ready.json" /digest)
  export SSL_CERT_FILE="$CASE_ROOT/registry/ca.pem"
}

lazy_stop_registry() {
  kill "$LAZY_REGISTRY_PID"
  wait "$LAZY_REGISTRY_PID" || test "$?" -eq 143
  unset LAZY_REGISTRY_PID
}

lazy_start_repository() {
  cp "$PVISOR_PROJECT_ROOT/tests/spec/lazy-image/regression/repository.py" "$CASE_ROOT/repository.py"
  python3 "$CASE_ROOT/repository.py" "$1" \
    --ready "$CASE_ROOT/repository-ready.json" \
    --requests "$CASE_ROOT/requests.jsonl" --deny "$CASE_ROOT/repository-deny" \
    > "$CASE_ROOT/repository.stdout" 2> "$CASE_ROOT/repository.stderr" &
  LAZY_REPOSITORY_PID=$!
  lazy_wait_ready "$CASE_ROOT/repository-ready.json" "$LAZY_REPOSITORY_PID"
  LAZY_REPOSITORY=$(journey_json "$CASE_ROOT/repository-ready.json" /url)
}
