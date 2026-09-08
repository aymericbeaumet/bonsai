---
name: bonsai
description: Manage git worktrees with the bonsai CLI - create isolated per-branch worktrees outside the repo, jump between them, and clean up merged ones. Use when working on a branch in isolation, parallelizing tasks across worktrees, or tidying up worktrees and branches.
---

## Git worktrees (bonsai)

### Scope and authorization

Use bonsai to complete the requested worktree operation. Explicit user
instructions override this skill's defaults; carry existing authorization
forward within its scope. Infer routine choices from context and continue
through verification rather than stopping at a proposed command.

If a skill instruction would block or redirect requested work, first check
whether it applies and whether the user already authorized the action. If
it still blocks progress, link the exact SKILL.md you read, quote the rule,
and explain the remaining blocker separately from your interpretation.

### Mental model

- Worktrees live outside the repository at `<root>/<repo-id>/<branch>`
  (default root `~/.bonsai`). Branch names nest: `feat/x` → `feat/x/`.
- Run bonsai from anywhere inside the repo — the main checkout or any
  worktree — it always operates on the repository as a whole.
- `add` preserves existing branch names and slugifies new inputs segment-by-segment
  while preserving `/` as a nested branch/path delimiter, creates branches automatically, and fetches
  the remote default branch first. Missing arguments open fuzzy pickers on a
  real terminal only; in non-interactive use, always pass arguments or the
  command exits with an error.

### Core workflow

```sh
path=$(bonsai add <branch>)     # create or reuse; prints absolute path; idempotent
cd "$path"                      # then work inside the worktree
bonsai add <branch> --base HEAD # stack on the checkout you run it from
bonsai list --json              # this repo's worktrees, machine-readable
bonsai remove <branch> [-d]     # drop the worktree (keep branch unless -d)
bonsai clean --dry-run --json   # inspect merged/squash-merged branches
bonsai clean --yes              # execute when cleanup is authorized
```

### Invariants

- `--base` resolves against the directory bonsai runs from, not the main
  checkout: `--base HEAD` inside a worktree stacks on that worktree.
- New branches created from a base carry no upstream (`--no-track`) until
  first push; branches checked out from the remote track that remote branch.
- A deleted upstream alone never proves a branch is safe to clean.
- A fresh worktree with zero commits already counts as "merged", so `clean`
  will list it — read the plan before executing.
- Untracked local config (`.env*`, `.envrc`, `.mcp.json`, `CLAUDE.local.md`,
  ...) is copied into new worktrees automatically.
- Dependencies are installed automatically in new worktrees when a lockfile
  is present (pnpm/npm/yarn/bun/cargo/uv, lockfiles preserved). Missing tools
  and failed installs leave the worktree available and print an incomplete-setup
  summary with a retry command. Re-adding does not rerun setup. Check stderr before
  assuming deps are in place. Follow any linked package-manager configuration
  warning to enable its worktree-optimized shared store.
- Shell auto-cd applies only to terminal stdout; substitutions and redirections
  preserve paths. `command bonsai` bypasses the wrapper explicitly.

### Cleanup and removal

- Inspect cleanup candidates with `bonsai clean --dry-run --json`.
  A preview-only request ends with the plan. When the user authorized
  cleanup and the inspected targets fit that scope, execute with `--yes`;
  the user need not name the flag. Preserve the preview's scope flags.
- `clean` removes eligible worktrees and their branches across the current
  repo. A fresh worktree with no commits can qualify; exclude active work
  from the intended cleanup. For named targets or a subset of the plan, use
  `bonsai remove <branch>`; add `-d` only when branch deletion is intended.
- `prune` has no dry-run mode. Inspect registrations and candidate directory
  contents before deleting orphans, which may contain uncommitted work.
  Keep `prune --all`, which spans repositories, within the user's scope.
- On `clean` and `prune`, `--force` is an alias of `--yes`: it skips the
  prompt. `clean` still skips dirty worktrees. On `remove`, `--force` can
  discard uncommitted files and, with `-d`, delete an unmerged branch. Use
  it only when that data loss is authorized; a refusal alone is not
  authorization to force removal.
- If authorization is missing, prepare the candidate list before asking
  about the specific removal or data loss. Do not expand a cleanup request
  to unrelated worktrees or repositories.

### Verify the result

After adding a worktree, use the returned path and confirm its branch before
editing. After cleanup or removal, inspect `bonsai list --json` and the
command's report; report removed targets, dirty skips, and failures accurately.
If an operation partly succeeds, inspect the remaining state before retrying.
Read dependency-install warnings on stderr even when `add` succeeds.

Use `bonsai <cmd> --help` for the installed version's exact interface.
