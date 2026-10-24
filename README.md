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
│ 0:editor ·◐●   1:atc ◐●◐ -   2:dotfiles ! *                  │
└──────────────────────────────────────────────────────────────┘
```

One row — tmux's own window list, with one glyph per AI pane added inside each
window name. tmux keeps its flags and its current-window highlight; atc only
fills in the glyphs, through a window option, so nothing is rendered per redraw.

Colour is on by default. For shape and weight only:

```sh
set -g @atc_colour off
```

Bound to a key if you want to flip it while looking at the bar:

```sh
bind-key A if-shell -F '#{==:#{@atc_colour},off}' \
  'set -g @atc_colour on' 'set -g @atc_colour off'
```

Sessions in *other* tmux sessions are not on the bar — the command center
groups by session and is the place to see all of them at once. If you would
rather have the old second row, which does show them, add:

```sh
set -g status 2
set -g status-format[1] "#[align=left] #(atc bar #{session_name})"
```

`prefix + a` opens the command center over the top of your layout — nothing
resizes, and `q` dismisses it:

```
┌─ atc — 4 session(s), 2 need you ─────────────────────────────┐
│ myworkspace                                                  │
│   0 editor                                                   │
│     ·  nvim                tool                  nvim        │
│     ◐  cargo               job                   atc/main    │
│   1 atc                                                      │
│ ▌ ●  fix-pyright-lint      waiting    4m  dotfiles/main      │
│     ◐  atc-command-center  working    8s  atc/main           │
│        ⑂ Explore                 1m                          │
│        ⑂ general-purpose        12s                          │
│   2 review                                                   │
│     !  coding-standards    blocked   45s  atc/main           │
│     ●~ add-k9s-dotfiles    waiting    ~   dotfiles/main      │
├─ myworkspace:1.0 · dotfiles/main ────────────────────────────┤
│ test result: ok. 42 passed; 0 failed                         │
│ Fixed the 3 type errors. Want me to run the tests?           │
├──────────────────────────────────────────────────────────────┤
│ enter jump  p prompt  x kill  tab other panes  q close       │
└──────────────────────────────────────────────────────────────┘
```

Grouped the way your tmux is: session, then window, then the panes in it. So a
session always appears next to whatever it shares a window with, and you can see
at a glance which terminal belongs to what. Windows lead with whatever is most
urgent, and the session you are attached to comes first, so the thing that wants
you still rises to the top.

The bottom half is the live screen of whichever pane is selected, so you can read
what a session actually said — and answer it with `p` — without going there.
Everything is jumpable, so the popup is also how you get back to the `nvim` or
`k9s` pane.

`⑂` rows are the subagents running under a session, with how long each has been
going. They are not panes, so `j`/`k` step over them and `enter`, `p` and `x`
never land on one.

`~` means the state was read off the pane's own screen rather than reported by a
hook — see [Sessions that never reported](#sessions-that-never-reported).

## Sessions that never reported

A session that started before atc was watching has fired no hook, so there is
nothing on disk for it. Rather than show it as unknown, atc reads its screen:
the interrupt hint means it is working, a permission question with numbered
options means it is blocked, and a bare input box means it is waiting for you.
Anything it does not recognise stays unknown.

These are guesses about Claude Code's current interface, not a contract, so they
are marked `~` everywhere they appear and a real hook always wins. Nothing
inferred is ever written to disk.

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

## What it knows about

The pane list comes from tmux, and hook state is laid over it. So every pane is
accounted for, not just the ones that have reported:

| Kind | What it is | On the fleet row |
|---|---|---|
| `session` | an AI session with hook state | its state glyph |
| `unreported` | looks like an AI session but has never fired a hook | `?` |
| `tool` | a long-lived TUI: `nvim`, `k9s`, `tredis`, `lazygit`, `htop`… | only with `--all` |
| `shell` | an idle shell | only with `--all` |
| `job` | a foreground command in a shell pane: `cargo`, `pytest`… | only with `--all` |

`unreported` matters: a session started before `atc` was installed has no state,
and silently omitting it would hide a session that is genuinely waiting for you.
It resolves itself on that session's next activity.

`job` costs nothing to detect — a shell pane whose running command is no longer
a shell has a job in it, and when it goes back to a shell the job is done. That
gives you progress on non-AI work with no instrumentation. `tool` exists so
that `k9s` and `nvim` don't read as jobs that never finish.

Non-AI panes stay **off** the fleet row by default. The row exists to show which
session needs you, and a glyph for every shell would bury that.

## How it works

No daemon and no polling of pane contents.

- Claude Code hooks run as subprocesses that inherit the session's environment,
  so `$TMUX_PANE` identifies which pane fired the event. That is the whole
  mechanism for knowing what every session is doing.
- Claude panes report `pane_current_command` as a version string (`2.1.308`)
  rather than `claude`, so unreported sessions are spotted by the `✳` prefix
  Claude Code puts on the pane title.
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
| `atc bar [session] [--all]` | Render the fleet row; `--all` includes non-AI panes |
| `atc status [--json]` | Report every tracked session |
| `atc ui` | Open the command center |
| `atc sync` | Refresh every window's glyphs (tmux runs this each redraw) |
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
