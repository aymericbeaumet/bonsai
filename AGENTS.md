# Repository guidance

- Keep worktree guidance in `skills/bonsai/SKILL.md` as the single source for
  `bonsai skill`, installation, and `bonsai agents`. Keep its body ready to
  append to AGENTS.md with a level-two heading and deeper subsections.
- Keep the bundled skill self-contained: installation distributes only
  SKILL.md. Adding references or helpers also requires embedding, installing,
  and verifying those resources.
- Keep skill authorization rules tied to the requested operation and its
  effects. Preserve existing user authorization; do not require users to name
  confirmation flags. Distinguish confirmation bypass in clean/prune from
  data loss through remove --force.
- Verify agent-facing changes with the existing CLI tests for printed and
  installed content. Test their shared-source relationship rather than
  pinning prose fragments that would make editorial changes brittle.
- Before finishing, run the CI checks: `cargo fmt --check`,
  `cargo clippy --all-targets -- -D warnings`, and `cargo test`.
- The browser workspace is embedded from `web/dist`; after editing `web/src`,
  run `npm ci` and `npm run check` in `web/` and include the rebuilt assets.
  Rust builds must remain usable without Node.js. See
  [browser architecture](docs/browser-workspace.md) for API and lifecycle rules.
- Bonsai commands launched from HQ must use the executable and real PTYs so CLI
  configuration, prompts, and worktree ownership rules remain authoritative.
  Keep HTTP and WebSocket authentication, loopback Host/Origin checks, and
  terminal cleanup covered when changing server routes.
- Use tmux's native session operations and preserve its shell configuration;
  do not add persistent startup files or a Bonsai shell launcher for tmux.
- Keep worktree inventory independent of runtime activity. Runtime adapters
  annotate existing checkouts; match pane directories to the deepest canonical
  worktree and keep idle worktrees visible.
- Capture the working directory and exact target path when opening an HQ
  action dialog. Inventory refreshes must never retarget a pending command
  or removal confirmation.
- `BONSAI_*` environment variables are strict configuration keys. Prefix
  internal process/bootstrap/test variables with `_BONSAI_` so they cannot
  make normal commands fail configuration parsing.
- Repository discovery caches the initial worktree inventory only within one
  invocation. `Repo::lock_mutations` must invalidate it: an interactive wait or
  another Bonsai process may have changed the inventory before lock acquisition.
- Workspace folder ownership lives in `.locks/workspace-*.folders.json`, under
  the corresponding workspace lock. Preserve unknown entries; commit workspace
  content before ownership metadata so interruption cannot create false ownership.
- Cwd recovery belongs to operation outcomes, including partial failures.
  Unwrapped `clean --json` carries recovery inside its single JSON document;
  shell-wrapped navigation uses the sentinel protocol.
- `REQUIRE_TEST_SHELLS=1` makes Bash/Zsh/Fish coverage mandatory.
- Release PTY checks are in `tests/terminal.py`; scalable disposable benchmarks
  are in `scripts/benchmark.py`. The two real package-manager CLI tests are
  ignored in ordinary runs and must be requested with `--ignored`.
