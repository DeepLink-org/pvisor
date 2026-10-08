#!/usr/bin/env bash
set -u

echo '1. Create a file in the staged workspace'
touch created-by-agent.txt

echo '2. Try to change a protected file'
if touch private/token 2>/dev/null; then
  echo 'UNEXPECTED: protected file was writable'
  exit 1
else
  echo 'DENIED: touch private/token'
fi

echo '3. Try to reach a blocked destination through the injected proxy'
proxy="${http_proxy:-${HTTP_PROXY:-}}"
if [[ -z "$proxy" ]]; then
  echo 'ERROR: pVisor did not inject an HTTP proxy' >&2
  exit 1
fi
if curl --fail --silent --show-error --noproxy '' --proxy "$proxy" \
  --max-time 3 http://blocked.example/; then
  echo 'UNEXPECTED: blocked destination was reachable'
  exit 1
else
  echo 'DENIED: curl http://blocked.example/'
fi

if [[ "${1:-}" == '--once' ]]; then
  exit 0
fi

echo
echo 'Open the Files panel with Ctrl-] then f; Network with Ctrl-] then n.'
echo 'Close a panel with Esc. Type exit to finish the Job.'
exec bash --noprofile --norc -i
