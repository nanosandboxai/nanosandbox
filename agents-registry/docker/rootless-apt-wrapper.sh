#!/bin/sh
# Rootless apt-get/apt wrapper for nanosandbox code agents.
#
# Forwards package manager commands to agent-gateway's /api/v1/pkg/install
# endpoint, which runs them as root. Output streams in real-time.
#
# This wrapper is placed at /usr/local/bin/apt-get and /usr/local/bin/apt,
# taking PATH precedence over /usr/bin/apt-get and /usr/bin/apt.

CMD="$(basename "$0")"

# Build JSON args array
ARGS_JSON="["
FIRST=true
for arg in "$@"; do
    if [ "$FIRST" = true ]; then
        FIRST=false
    else
        ARGS_JSON="$ARGS_JSON,"
    fi
    escaped=$(printf '%s' "$arg" | sed 's/\\/\\\\/g; s/"/\\"/g')
    ARGS_JSON="$ARGS_JSON\"$escaped\""
done
ARGS_JSON="$ARGS_JSON]"

# Stream output, capture exit code from last line
EXIT_CODE=1
curl -sN -X POST http://localhost:8080/api/v1/pkg/install \
    -H "Content-Type: application/json" \
    -d "{\"command\":\"$CMD\",\"args\":$ARGS_JSON}" 2>/dev/null | \
while IFS= read -r line; do
    case "$line" in
        __EXIT_CODE__:*)
            EXIT_CODE="${line#__EXIT_CODE__:}"
            exit "$EXIT_CODE"
            ;;
        *)
            printf '%s\n' "$line"
            ;;
    esac
done

# If curl itself failed (gateway not reachable)
exit "${EXIT_CODE:-1}"
