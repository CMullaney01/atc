#  vhs docs/demo/row.tape
#
# Close-ups of the status row alone, in colour and in shape-only. Deliberately
# a short terminal: the row is the subject, not the panes above it.

# The stills are the point here; the GIF is a by-product, so it is written
# outside the repo rather than committed.
Output /tmp/atc-row-toggle.gif

Require tmux

Set Shell "bash"
Set FontFamily "JetBrainsMono Nerd Font Mono"
Set FontSize 15
Set Width 1300
Set Height 300
Set Padding 16
Set Theme "Catppuccin Mocha"
Set Framerate 24

Hide
Type "cd ~/dev/atc && bash docs/demo/stage.sh >/dev/null 2>&1 && clear" Enter
Sleep 14s
Type "tmux -L atcdemo attach -t myworkspace" Enter
Sleep 4s
Show

# The editor window, so the panes above the row are a real editor and a real
# build rather than wrapped prose. The row still carries every window.
Ctrl+A
Type "0"
Sleep 3s
Screenshot docs/img/row.png

# Shape and weight only, for a bar that is already busy or already coloured.
Ctrl+A
Type ":"
Sleep 800ms
Type "set -g @atc_colour off"
Enter
Sleep 3s
Screenshot docs/img/row-mono.png
Sleep 1s

Ctrl+A
Type ":"
Sleep 500ms
Type "set -g @atc_colour on"
Enter
Sleep 2500ms
