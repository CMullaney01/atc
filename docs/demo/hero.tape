#  vhs docs/demo/hero.tape
#
# The README's hero GIF and the command-center stills. Everything on screen is
# a real tmux session whose state was written by the agents' own hooks -- see
# stage.sh. Nothing here is mocked up.

Output docs/img/hero.gif

Require tmux

Set Shell "bash"
Set FontFamily "JetBrainsMono Nerd Font Mono"
Set FontSize 13
Set Width 1500
Set Height 810
Set Padding 18
Set Theme "Catppuccin Mocha"
Set TypingSpeed 45ms
Set Framerate 24

Hide
Type "cd ~/dev/atc && bash docs/demo/stage.sh >/dev/null 2>&1 && clear" Enter
Sleep 14s
Type "tmux -L atcdemo attach -t myworkspace" Enter
Sleep 5s
Show

# Two claude sessions side by side, and the row underneath carrying every
# window's panes at once.
Sleep 3s

# prefix + a
Ctrl+A
Type "a"
Sleep 3s
Screenshot docs/img/popup.png

# The cursor opens on whatever is most urgent. Walking down it, the bottom
# half is the selected pane's live screen, so the preview follows.
Type "j"
Sleep 2s
Type "j"
Sleep 2s
Type "j"
Sleep 2s
Type "j"
Sleep 2s

# tab drops everything that is not an agent.
Tab
Sleep 2500ms
Screenshot docs/img/popup-agents.png
Tab
Sleep 1500ms

# Back up to the blocked one and jump straight into it. It is in another
# window, so the layout behind the popup changes with you.
Type "k"
Sleep 500ms
Type "k"
Sleep 500ms
Type "k"
Sleep 500ms
Type "k"
Sleep 1800ms
Enter
Sleep 3500ms
Screenshot docs/img/jumped.png
Sleep 1500ms
