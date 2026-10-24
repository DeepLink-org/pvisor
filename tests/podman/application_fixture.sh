#!/bin/sh
set -eu
if [ "$1" = web ]; then
    count=$(cat /boots 2>/dev/null || echo 0)
    count=$((count + 1))
    printf '%s\n' "$count" > /boots
    printf 'APP_BOOT %s\n' "$count"
    printf 'APP_ERR %s\n' "$count" >&2
    exec /app-http
fi
test "$1" = idle
printf 'APP_IDLE\n'
exec sleep 7200
