# Records demo.gif with VHS (https://github.com/charmbracelet/vhs).
# Run ./record.sh rather than vhs directly: it builds the container this
# tape expects, with bonsai built from the checkout.

Output demo.gif

Set Shell bash
Set FontSize 16
Set Width 1280
Set Height 680
Set Padding 24
Set Theme "Catppuccin Mocha"
Set WindowBar Colorful
Set BorderRadius 10
Set TypingSpeed 45ms

Hide
Type ". ./setup.sh && exec zsh" Enter
Sleep 3s
Ctrl+L
Sleep 500ms
Show

Sleep 1s
Type "bonsai add fix-login"
Type@15ms "   # new branch + worktree, and you're in it"
Sleep 600ms Enter
Sleep 2.5s

Type `echo "export const next = '/home'" >> login.js`
Sleep 300ms Enter
Type `git commit -qam "fix: redirect after login" && git push -q`
Sleep 400ms Enter
Sleep 1.5s
Type@15ms "# ...the pull request gets squash-merged on GitHub"
Sleep 600ms Enter
Sleep 1.5s

Type "bonsai list"
Sleep 400ms Enter
Sleep 3s

Type "bonsai cd"
Type@15ms "   # fuzzy-jump, most recently used first"
Sleep 600ms Enter
Sleep 2s
Type@120ms "dark"
Sleep 1s Enter
Sleep 2.5s

Type "bonsai clean"
Type@15ms "   # remove merged and squash-merged worktrees"
Sleep 600ms Enter
Sleep 3s
Enter
Sleep 4s
