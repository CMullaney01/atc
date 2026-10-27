#!/usr/bin/env sh
# One demo pane: report state through the real hook, then sit there showing a
# plausible screen. Run as a pane's command, so TMUX_PANE is the pane's own and
# the hook writes state for it exactly as the agent itself would.
#
#   agent.sh <tool> <title> <screen> <event>...
set -eu

ATC="${ATC:-$HOME/dev/atc/target/release/atc}"
HERE=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
DEMO_BIN=${DEMO_BIN:?stage.sh exports this}

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
    # Not swallowed. A hook that fails here is the demo quietly turning into a
    # fleet of ordinary jobs, which is a thing to notice rather than record.
    printf '{"session_id":"demo-%s","cwd":"%s","session_title":"%s","notification_type":"%s"}' \
        "$title" "$PWD" "$title" "$kind" |
        "$ATC" hook --tool "$tool" "$event" >/dev/null
done

clear
cat "$HERE/screens/$screen"

# The pane now has to look like the agent as well as report like one. atc only
# believes state while the pane could still be running the thing that wrote
# it, and it goes by the process name, so sitting on `sleep` here would read
# as an ordinary job. stage.sh built a copy under each of these names.
case $tool in
    claude) procname=2.1.308 ;;
    *) procname=$tool ;;
esac
exec "$DEMO_BIN/$procname"
