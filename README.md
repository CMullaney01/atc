# atc

Air traffic control for tmux AI coding sessions.

Run ten Claude sessions across tmux panes and you lose track of them. Nothing
tells you which one has stopped and is waiting for a reply, which is blocked on
a permission prompt, and which is still working — so you end up cycling through
panes to look. `atc` puts that on your status bar and behind one keystroke.

```
┌──────────────────────────────────────────────────────────────┐
│  (your panes — nothing moves, nothing resizes)               │
├──────────────────────────────────────────────────────────────┤
│ [myworkspace] 0:editor! 1:atc◐ 2:binance                     │  ← window names flagged
│ 0:editor ●!  1:atc ◐  │  [review] 0:zsh ✓                    │  ← the fleet row
└──────────────────────────────────────────────────────────────┘
```

One cluster per window, one glyph per pane. A flagged window name tells you
where to go; the fleet row tells you which pane inside it, including panes in
tmux sessions you aren't attached to.

`prefix + a` opens the command center over the top of your layout — nothing
resizes, and `q` dismisses it:

```
┌─ atc — 3 session(s), 2 need you ─────────────────────────────┐
│ ▌● fix-pyright-lint   waiting    4m  myworkspace:0.1  dotfiles/main
│      └ "Fixed the 3 type errors. Want me to run the tests?"  │
│  ! coding-standards   blocked   45s  myworkspace:0.2  atc/main
│  ◐ atc-command-center working    8s  myworkspace:1.0  atc/v1 │
├──────────────────────────────────────────────────────────────┤
│ enter jump   p prompt   x kill   r refresh   q close         │
└──────────────────────────────────────────────────────────────┘
```

Most urgent first, so whatever needs you is always the top row.

## States

| Glyph | State | Set by |
|---|---|---|
| `●` | waiting — finished, wants a reply | `Stop` |
| `◐` | working | `UserPromptSubmit`, `PreToolUse`, `PostToolUse` |
| `!` | blocked — needs an approval | `PermissionRequest` |
| `✗` | error | `StopFailure`, `PostToolUseFailure` |
| `·` | idle — registered, nothing pending | `SessionStart` |

Bold means you've been ignoring it for a minute (`Notification`). Visiting the
pane clears the bold; the glyph itself stays until the session gets a reply.

`Notification` is deliberately **not** the primary signal: its
`permission_prompt` matcher fires ~6s after the prompt appears and `idle_prompt`
~60s after the session goes quiet. `Stop` and `PermissionRequest` fire
immediately, so they drive state and `Notification` only escalates.

## How it works

No daemon and no polling of pane contents.

- Claude Code hooks run as subprocesses that inherit the session's environment,
  so `$TMUX_PANE` identifies which pane fired the event. That is the whole
  mechanism for knowing what every session is doing.
- State lives in `~/.local/state/atc/panes/` as one small JSON file per pane,
  written atomically.
- The hook also writes tmux user options — `@atc_state` on the pane and an
  aggregate `@atc_win` on its window — which the status format reads directly,
  so window glyphs cost nothing to draw.
- Every hook ends with `refresh-client -S`, so the bar updates the instant a
  session stops rather than on the next status interval.
- The fleet row reconciles against `tmux list-panes` on every render, so a
  killed pane can't leave a ghost glyph behind.

## Install

Needs tmux 3.0+ (tested on 3.7) and Rust.

```sh
git clone https://github.com/CMullaney01/atc ~/dev/atc
cd ~/dev/atc
cargo build --release
ln -sfn "$PWD/target/release/atc" ~/.local/bin/atc   # anywhere on PATH

# tmux: source the config, or paste it into your own tmux.conf
echo 'source-file ~/dev/atc/tmux/atc.conf' >> ~/.tmux.conf
tmux source-file ~/.tmux.conf

# Claude Code: print the hook block and merge it into ~/.claude/settings.json
atc install
atc doctor
```

`atc` must be on the **tmux server's** PATH, which it inherits from wherever
the server was started. `atc doctor` will tell you if it isn't.

## Commands

| Command | Purpose |
|---|---|
| `atc hook <Event> [--kind <matcher>]` | Apply a hook event (reads JSON on stdin) |
| `atc bar [session]` | Render the fleet row |
| `atc status [--json]` | Report every tracked session |
| `atc ui` | Open the command center |
| `atc state <status> [--title <t>]` | Report state for the current pane by hand |
| `atc seen <pane-id>` | Drop a pane's escalation flag |
| `atc ask <question>` | Ask Claude about the fleet |
| `atc doctor` | Check the installation |
| `atc install` | Print the tmux and hook configuration |

## Anything that isn't Claude

`atc state` is the generic entry point, so any tool or long-running job can
report in:

```sh
atc state working --title "cargo bench"
cargo bench
atc state waiting
```

`atc status --json` exposes the whole fleet, which is also how a "manager"
session can reason about it:

```sh
atc ask "which session has been waiting longest, and what did it last say?"
```

## Licence

MIT
