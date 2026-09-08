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
- Repository discovery caches the initial worktree inventory only within one
  invocation. `Repo::lock_mutations` must invalidate it: an interactive wait or
  another Bonsai process may have changed the inventory before lock acquisition.
- Workspace folder ownership lives in `.locks/workspace-*.folders.json`, under
  the corresponding workspace lock. Preserve unknown entries; commit workspace
  content before ownership metadata so interruption cannot create false ownership.
- Cwd recovery belongs to operation outcomes, including partial failures.
  Unwrapped `clean --json` carries recovery inside its single JSON document;
  shell-wrapped navigation uses the sentinel protocol.
- `REQUIRE_TEST_SHELLS=1` makes shell coverage mandatory. Do not use a `BONSAI_`
  prefix for test-control variables: that namespace is parsed as configuration.
- Release PTY checks are in `tests/terminal.py`; scalable disposable benchmarks
  are in `scripts/benchmark.py`. The two real package-manager CLI tests are
  ignored in ordinary runs and must be requested with `--ignored`.
