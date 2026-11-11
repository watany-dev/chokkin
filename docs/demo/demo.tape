# README demo GIF. Regenerate from the repository root with
# charmbracelet/vhs (https://github.com/charmbracelet/vhs):
#
#   vhs docs/demo/demo.tape
#
# The demo project comes from docs/demo/setup.sh; see the comment there.

Output docs/demo/demo.gif

Set Shell "bash"
Set FontSize 14
Set Width 1520
Set Height 640
Set Padding 20
Set TypingSpeed 40ms
Set Theme "Dracula"

Hide
Type "rm -rf /tmp/chokkin-demo && docs/demo/setup.sh /tmp/chokkin-demo && cd /tmp/chokkin-demo"
Enter
# Warm the uv cache so the first run is not a download.
Type "uvx chokkin --version >/dev/null 2>&1 && export PS1='$ ' && clear"
Enter
Show

Type "uvx chokkin"
Sleep 500ms
Enter
Sleep 5s

Type "clear"
Enter
Type "uvx chokkin --explain CHK002:requests"
Sleep 500ms
Enter
Sleep 5s

Type "clear"
Enter
Type "uvx chokkin --fix --dry-run"
Sleep 500ms
Enter
Sleep 6s
