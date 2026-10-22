# The short-lived subshell leaves sleep adopted by PID 1 (or the test subreaper).
i=0
while [ "$i" -lt 20 ]; do
    (sleep 0.01 &)
    i=$((i + 1))
done
i=0
while [ -n "${WAIT_FOR:-}" ] && [ ! -f "$WAIT_FOR" ]; do
    i=$((i + 1))
    [ "$i" -lt 1000 ] || exit 43
    sleep 0.01
done
sleep 0.2
for status in /proc/[0-9]*/status; do
    state= parent=
    while read -r key value rest; do
        case "$key" in
            State:) state=$value ;;
            PPid:) parent=$value ;;
        esac
    done < "$status" 2>/dev/null || continue
    if [ "$parent" = "$PPID" ] && [ "$state" = Z ]; then
        echo "unreaped orphan: $status" >&2
        exit 42
    fi
done
