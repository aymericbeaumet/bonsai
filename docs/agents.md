# AI coding agents

bonsai is built to work the same across coding harnesses (Claude Code,
Codex, OpenCode, Cursor, ...), so switching tools mid-project costs nothing.
It sticks to the two cross-harness standards — no per-harness plugins to
install or maintain.

## Agent Skill

The [Agent Skill](https://agentskills.io) (`skills/bonsai/SKILL.md`)
teaches agents the workflow, its invariants, and how to complete authorized
operations while preserving cleanup scope and uncommitted work. It is
embedded in the binary and installs into each harness's
standard skill directory:

```sh
bonsai skill install        # detects Claude Code, Codex, OpenCode, Cursor, Pi
bonsai skill install --all  # or install for every harness unconditionally
bonsai skill                # or print it and pipe it wherever you want
```

## AGENTS.md

`bonsai agents >> AGENTS.md` appends the same skill body without YAML
frontmatter to the cross-harness instructions file. Both commands use
`skills/bonsai/SKILL.md` embedded at build time, so their guidance stays in
sync. After upgrading bonsai, rerun `bonsai skill install` to refresh
installed copies, or replace the existing bonsai section in `AGENTS.md` with
fresh `bonsai agents` output.

## Scripting

`path=$(bonsai add feat-x)` prints the worktree path and is idempotent;
`bonsai list --json` and `bonsai clean --dry-run --json` give structured
output for a reliable inspect-then-execute loop; `clean --yes` /
`prune --yes` (alias `-f`/`--force`) never prompt; fuzzy pickers only ever
appear on a real terminal.

New worktrees come pre-provisioned: untracked local config — `.env*`,
`.envrc`, `.mcp.json`, `CLAUDE.local.md`, `.claude/settings.local.json`,
`.cursor/mcp.json` — is copied over by default, and dependencies are
installed automatically from the lockfile, so whichever harness (or human)
created the worktree, the others find their setup in place.

## Start sessions

`bonsai start [claude|codex|opencode]` starts a new coding session in the
current worktree. When the provider is omitted, bonsai offers the installed
ones, or picks the only one installed. `--prompt` supplies an initial prompt,
and `-a` picks a worktree from any project first.

## Resume sessions

Run `bonsai resume` from any checkout or registered worktree of a project to
search its resumable top-level Claude Code, Codex, and OpenCode history in one
picker. The UI shares `bonsai cd`'s keyboard navigation, type-to-filter
behavior, relative ages, freshness colors, and recent-first ordering. Search
runs asynchronously as you type; arrow keys and Ctrl-P/N navigate, while
standard editing keys such as Ctrl-W/A/E/U edit the filter. Session titles,
providers, IDs, branches, and directories are all searchable. Both pickers
render inline below the current shell prompt and clear themselves on exit.

```sh
bonsai resume                 # newest session is selected by default
bonsai resume parser          # unique matches open; otherwise pre-filter
bonsai resume <session-id>    # exact IDs resume immediately
```

The selected harness starts in the session's original directory. If an old
worktree has been removed, bonsai warns and starts it from the main checkout.
Standard location overrides are respected: `CLAUDE_CONFIG_DIR`, `CODEX_HOME`,
`CODEX_SQLITE_HOME`, `XDG_DATA_HOME`, and `OPENCODE_DB`.
