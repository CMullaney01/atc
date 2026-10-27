#!/usr/bin/env bash
# Builds the fleet the README screenshots are taken of: two tmux sessions, all
# four agents, each one's state written by its own real hook rather than faked.
#
# Runs on its own tmux socket, so recording this cannot disturb a live session.
# Anything inside a pane inherits that socket through $TMUX, which is why atc
# reports on the demo fleet and not on the real one.
set -euo pipefail

REPO=${REPO:-$HOME/dev/atc}
ATC=${ATC:-$REPO/target/release/atc}
HERE=$REPO/docs/demo
SOCKET=atcdemo
TM="tmux -L $SOCKET"
AGENT="$HERE/agent.sh"

# Its own state directory, which matters more than it looks. State is keyed by
# tmux pane id, and ids are per-server, so the real server's status bar prunes
# every file it does not recognise -- including this fleet's, mid-recording.
export ATC_STATE_DIR=${ATC_STATE_DIR:-${TMPDIR:-/tmp}/atc-demo-state}
export ATC
rm -rf "$ATC_STATE_DIR"
mkdir -p "$ATC_STATE_DIR"

# A demo pane has to be detectable as its agent, not just report like one, so
# each one runs a binary named the way that agent names itself. Claude's is
# named after its version, which is the whole reason atc has a rule for that.
export DEMO_BIN=${DEMO_BIN:-${TMPDIR:-/tmp}/atc-demo-bin}
rm -rf "$DEMO_BIN"
mkdir -p "$DEMO_BIN"
RUSTC=${RUSTC:-$(command -v rustc || echo "$HOME/.cargo/bin/rustc")}
"$RUSTC" -O -o "$DEMO_BIN/base" "$HERE/sleeper.rs"
for name in 2.1.308 codex gemini copilot; do
    cp "$DEMO_BIN/base" "$DEMO_BIN/$name"
done
rm "$DEMO_BIN/base"

$TM kill-server 2>/dev/null || true
sleep 0.3

agent() { # <target> <tool> <title> <screen> <event>...
    local target=$1
    shift
    $TM respawn-pane -k -t "$target" "$AGENT $*"
}

# The server inherits the exported variable, so its status bar and every pane
# it spawns read the same isolated state.
# tmux takes a path and nothing else, so the two in the config are filled in
# here rather than written down. Otherwise the demo only records on the one
# machine whose home directory got committed.
CONF=$DEMO_BIN/demo.conf
sed -e "s|__REPO__|$REPO|g" -e "s|__ATC__|$ATC|g" "$HERE/demo.conf" > "$CONF"

$TM -f "$CONF" new-session -d -s myworkspace -n editor -c "$REPO" -x 200 -y 50
$TM set-environment -g ATC_STATE_DIR "$ATC_STATE_DIR"
$TM set-environment -g DEMO_BIN "$DEMO_BIN"

# A real editor and a real git TUI, so the rows atc calls "tool" are genuinely
# that rather than something dressed up as one.
$TM send-keys -t myworkspace:editor "nvim src/process.rs" Enter
$TM split-window -h -t myworkspace:editor -c "$REPO"
# A genuinely long build: a cold target directory, so every dependency is
# compiled and the job is still running when the recording ends. atc spots a
# job in a shell pane with no instrumentation at all, and this is that.
BUILD_DIR=$(mktemp -d "${TMPDIR:-/tmp}/atc-demo-build.XXXXXX")
# Single-threaded on purpose: a parallel cold build finishes in fifteen
# seconds, which is not long enough to still be a job when the shutter opens.
$TM send-keys -t myworkspace:editor.1 \
    "cargo build --release -j 1 --target-dir $BUILD_DIR" Enter

$TM new-window -d -t myworkspace -n atc -c "$REPO"
$TM split-window -h -t myworkspace:atc -c "$HOME/dev/clitools"
agent myworkspace:atc.0 claude add-tool-flag claude-working.txt SessionStart UserPromptSubmit PreToolUse
agent myworkspace:atc.1 claude fix-pyright-lint claude-waiting.txt SessionStart Stop

$TM new-window -d -t myworkspace -n review -c "$HOME/dev/pa"
$TM split-window -h -t myworkspace:review -c "$HOME/dev/aeron-cgo"
$TM split-window -v -t myworkspace:review.1 -c "$HOME/dev/dotfiles"
agent myworkspace:review.0 codex lint-config codex-blocked.txt SessionStart PermissionRequest
agent myworkspace:review.1 gemini rate-limits gemini-waiting.txt SessionStart AfterAgent
agent myworkspace:review.2 copilot k9s-aliases copilot-working.txt sessionStart preToolUse

# A second session, so the command center has more than one to group by. This
# one's claude has been waiting long enough to be escalated, which is the bold
# "you have been ignoring this" weight on the row.
$TM new-session -d -s dotfiles -n k9s -c "$HOME/dev/dotfiles"
$TM send-keys -t dotfiles:k9s "lazygit" Enter
$TM split-window -h -t dotfiles:k9s -c "$HOME/dev/dotfiles"
agent dotfiles:k9s.1 claude add-k9s-dotfiles claude-waiting.txt \
    SessionStart Stop Notification:idle_prompt

sleep 1
$TM select-window -t myworkspace:atc
$TM select-pane -t myworkspace:atc.0
# Through the server, so sync talks to this socket rather than the real one.
$TM run-shell "$ATC sync"

# Every glyph in the recording comes from one of those hooks, so a hook that
# did not run is a recording of the wrong thing. Six agents were staged; if
# six did not report, say so rather than record a fleet of jobs.
AGENTS=6
reported=$(find "$ATC_STATE_DIR" -name '*.json' | wc -l | tr -d ' ')
if [ "$reported" -ne "$AGENTS" ]; then
    echo "only $reported of $AGENTS agents reported through their hooks" >&2
    exit 1
fi
echo "staged: $($TM list-panes -a | wc -l | tr -d ' ') panes, \
$reported agents reported, state in $ATC_STATE_DIR"
