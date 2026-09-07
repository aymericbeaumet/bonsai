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
