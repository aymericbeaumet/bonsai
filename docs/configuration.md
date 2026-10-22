# Configuration

bonsai works without any configuration. For each setting, the first value
found in this list wins:

1. CLI flags, such as `--root` and `--remote`.
2. `BONSAI_*` environment variables. Use `__` for nested settings, for
   example `BONSAI_CLEAN__FETCH=false`. Variables that do not name a bonsai
   setting, such as another tool's `BONSAI_URL`, are ignored.
3. `git config bonsai.*`. Repository-local values override global Git values.
4. The project's `.bonsai.toml`.
5. `$XDG_CONFIG_HOME/bonsai/config.toml`, or
   `~/.config/bonsai/config.toml` when `XDG_CONFIG_HOME` is unset.
6. Built-in defaults.

Use `.bonsai.toml` for configuration the team should share. Bonsai first
looks in the worktree you are currently using, so a branch can test a config
change before it is merged. If that worktree has no `.bonsai.toml`, Bonsai
uses the file from the main checkout.

Use `git config bonsai.*` for personal or per-clone overrides without editing
the shared file:

```sh
git config bonsai.root ~/src/worktrees        # this clone only
git config --global bonsai.clean.fetch false  # all of your clones
git config --add bonsai.add.copy ".env"       # repeat for list values
```

## All settings

The complete TOML shape, with its defaults and common overrides, is:

```toml
root = "~/.bonsai"          # where worktrees live
remote = "origin"           # optional: defaults to checkout.defaultRemote, then origin
default_branch = "main"     # optional: skip detection
workspace = true            # maintain a .code-workspace file per repo

[add]
fetch = true                # fetch --prune before creating (set false for offline use)
install = true              # auto-install deps (pnpm/npm/yarn/bun/cargo/uv)
clone_cargo_target = true   # copy-on-write clone Cargo's target/ into new worktrees
post_add = "mise install"   # runs after automatic dependency installation
# untracked files copied into new worktrees; setting this replaces the
# defaults: [".env", ".env.*", ".envrc", ".mcp.json", "CLAUDE.local.md",
#           ".claude/settings.local.json", ".cursor/mcp.json"]
copy = [".env*", ".envrc"]

[clean]
fetch = true                # fetch --prune before computing merged branches
protected = ["release/*"]   # branch globs never cleaned

[trim]
# ignored paths trim never deletes, on top of [add] copy and the built-in
# local state (.env*, .envrc, *.local*, databases, Terraform state, keys,
# .idea, .vscode); like gitignore, a glob without `/` matches at any depth
keep = ["fixtures/*.dump", "secrets"]

[hq]
scrolloff = 10              # rows kept above and below the cursor, as in vim
```
