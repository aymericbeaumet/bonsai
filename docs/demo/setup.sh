# Sourced by demo.tape inside the recording container. Builds a throwaway
# world: "GitHub" is a set of local bare repositories reached through a fake
# ssh, so remote URLs and repo ids look real without any network access.
set -eu

export HOME=/home/ada
mkdir -p "$HOME/src" "$HOME/.local/bin" /srv/git/acme
export PATH="$HOME/.local/bin:$PATH"

cat >"$HOME/.local/bin/fake-ssh" <<'EOF'
#!/bin/sh
# git@github.com:<owner>/<repo> is served from /srv/git/<owner>/<repo>.
for cmd; do :; done
cd /srv/git && exec sh -c "$cmd"
EOF
chmod +x "$HOME/.local/bin/fake-ssh"

git config --global user.name "Ada Lovelace"
git config --global user.email ada@example.com
git config --global init.defaultBranch main
git config --global push.autoSetupRemote true
git config --global core.sshCommand fake-ssh

git init --quiet --bare /srv/git/acme/webapp.git
# Every pushed feature branch is squash-merged at once, like an approved pull
# request, and its remote branch is deleted.
cat >/srv/git/acme/webapp.git/hooks/post-receive <<'EOF'
#!/bin/sh
n=0
while read -r _ new ref; do
  [ "$ref" = refs/heads/main ] && continue
  [ "$new" = 0000000000000000000000000000000000000000 ] && continue
  n=$((n + 1))
  subject=$(git log -1 --format=%s "$new")
  commit=$(git commit-tree "$new^{tree}" -p refs/heads/main -m "$subject (#$n)")
  git update-ref refs/heads/main "$commit"
  git update-ref -d "$ref"
done
EOF
chmod +x /srv/git/acme/webapp.git/hooks/post-receive

git clone --quiet git@github.com:acme/webapp.git "$HOME/src/webapp" 2>/dev/null
cd "$HOME/src/webapp"
printf '# webapp\n' >README.md
printf 'export function login(user) {\n  return session.start(user)\n}\n' >login.js
printf '.env\n' >.gitignore
git add . && git commit --quiet -m "feat: initial app" && git push --quiet
printf 'API_KEY=dev-secret\n' >.env

# Work already in progress on another branch, last touched two days ago.
dark=$(bonsai add feat/dark-mode 2>/dev/null)
printf ':root { color-scheme: dark }\n' >"$dark/dark.css"
git -C "$dark" add dark.css && git -C "$dark" commit --quiet -m "feat: dark mode"
touch -d '2 days ago' "$(git -C "$dark" rev-parse --git-dir)/index"
touch -d '9 days ago' .git/index

cat >"$HOME/.zshrc" <<'EOF'
setopt prompt_subst interactive_comments
autoload -Uz add-zsh-hook compinit vcs_info && compinit -u
zstyle ':vcs_info:git:*' formats ' %F{magenta}%b%f'
add-zsh-hook precmd vcs_info
# A blank line between commands, but not above the first prompt.
_gap() { (( _gaps++ )) && print }
add-zsh-hook precmd _gap
PROMPT=$'%F{cyan}%~%f${vcs_info_msg_0_}\n%F{green}❯%f '
eval "$(bonsai init zsh)"
EOF
set +eu
