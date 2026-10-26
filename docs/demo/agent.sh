#!/usr/bin/env sh
# One demo pane: report state through the real hook, then sit there showing a
# plausible screen. Run as a pane's command, so TMUX_PANE is the pane's own and
# the hook writes state for it exactly as the agent itself would.
#
#   agent.sh <tool> <title> <screen> <event>...
set -eu

ATC="${ATC:-$HOME/dev/atc/target/release/atc}"
HERE=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

tool=$1
title=$2
screen=$3
shift 3

# An event may carry the matcher the tool would have sent with it, written
# Event:kind, because a Notification only means something with one.
for spec in "$@"; do
    event=${spec%%:*}
    kind=${spec#"$event"}
    kind=${kind#:}
    printf '{"session_id":"demo-%s","cwd":"%s","session_title":"%s","notification_type":"%s"}' \
        "$title" "$PWD" "$title" "$kind" |
        "$ATC" hook --tool "$tool" "$event" >/dev/null 2>&1 || true
done

clear
cat "$HERE/screens/$screen"
# Nothing further to do: the pane only has to stay alive and keep its screen.
exec sleep 100000
