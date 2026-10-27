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

# It opens on the agent list: every session, worst first, nothing else in it.
Screenshot docs/img/popup.png
# The screenshots are written when the recording ends, so a key pressed right
# after one lands in it. Every shot needs room after it before the next key.
Sleep 2s

# Walking it, the bottom half is the selected pane's live screen, so the
# preview follows. These are the sessions you would be clearing.
Type "j"
Sleep 2s
Type "j"
Sleep 2s
Type "j"
Sleep 2s

# tab swaps to the tree: every pane, in the order tmux holds it.
Tab
Sleep 3s
Screenshot docs/img/popup-tree.png
Sleep 2s

# Back to the agent list, where `g` is the worst thing in the fleet because
# the list is sorted that way. Then jump straight into it -- it is in another
# window, so the layout behind the popup changes with you.
Tab
Sleep 2s
Type "g"
Sleep 2s
Enter
Sleep 3500ms
Screenshot docs/img/jumped.png
Sleep 1500ms
