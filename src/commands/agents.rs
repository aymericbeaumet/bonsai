/// Markdown usage contract for AI coding agents, ready to append to the
/// cross-harness AGENTS.md (read by Codex, Cursor, OpenCode, Claude Code,
/// Amp, Gemini CLI, ...): `bonsai agents >> AGENTS.md`.
pub fn run() {
    print!("{AGENTS_SNIPPET}");
}

const AGENTS_SNIPPET: &str = r#"## Git worktrees (bonsai)

This project manages git worktrees with `bonsai`: one isolated checkout per
task, stored outside the repository. Prefer a fresh worktree over switching
branches in place.

- Create (or get) a worktree: `path=$(bonsai add <branch>)`. Prints the
  worktree's absolute path on stdout; creates the branch from the default
  branch when it does not exist. Idempotent: re-running returns the existing
  path. Run all subsequent commands inside that directory.
- Stack on the current checkout: `bonsai add <branch> --base HEAD`.
- List worktrees: `bonsai list` (TSV: branch, path, flags) or
  `bonsai list --json`.
- Remove a worktree when done: `bonsai remove <branch>` (`-d` also deletes
  the branch; `--force` discards uncommitted changes).
- Remove merged/squash-merged worktrees: inspect with
  `bonsai clean --dry-run --json`, then execute with `bonsai clean --yes`
  (dirty worktrees are never touched).

Notes for non-interactive use:

- Always pass arguments explicitly; fuzzy pickers only appear on a real
  terminal, otherwise bonsai exits with an error asking for the argument.
- Every command works from inside any worktree of the repo and operates on
  the repository as a whole.
- Untracked local config (.env*, .envrc, .mcp.json, CLAUDE.local.md, ...) is
  copied into new worktrees automatically, so agent/harness setup carries
  over.
- Dependencies are installed automatically when a lockfile is present
  (pnpm/npm/yarn/bun/cargo/uv); install failures are reported on stderr but
  never abort `bonsai add`.
"#;

#[cfg(test)]
mod tests {
    #[test]
    fn snippet_mentions_the_core_contract() {
        for needle in ["bonsai add", "bonsai clean --yes", "stdout", "Idempotent"] {
            assert!(super::AGENTS_SNIPPET.contains(needle), "missing: {needle}");
        }
    }
}
