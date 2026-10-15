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
- The root package is `bonsai-cli` and builds the `bonsai` binary.
  `bonsai-lib` is the library, imported as `bonsai`. Keep both versions and
  the CLI's dependency requirement in sync. `cargo install --path .` installs
  the CLI. CLI sources and integration tests live in `bonsai-cli/`; library
  sources live in `bonsai-lib/`. The root package embeds `skills/bonsai/SKILL.md`
  so publication does not reach outside the library crate.
- Before finishing, run the CI checks: `cargo fmt --check`,
  `cargo clippy --all-targets -- -D warnings`, `cargo test`, and
  `cargo package --workspace --locked`.
- Trust `.mise.toml` with `mise trust .mise.toml` and run
  `mise install --locked rust node python uv` for development tools. Run checks
  through `mise exec --` unless mise is
  activated in the shell. Tool pins and platform checksums live in `.mise.toml`
  and `mise.lock`.
- `bonsai hq` is a terminal worktree list. Enter, `o`, and Ctrl-I open the
  selected project and list its checkouts by branch. Ctrl-O and Esc go up
  one step: clear a search, leave the command line, then leave the project.
  `:` runs
  add, remove, clean, and prune for that project, with the same confirmation
  rules as the CLI (`yes` stays false; remove does not pass `--force`).
  Inventory is Git checkouts under the bonsai root, workspace folder
  references, and the project where hq starts. Do not add a browser, a
  server, or a second UI. Do not run a shell or tmux inside hq.
- Paint the worktree list before git status. Status and footprints stream in
  afterwards. `--json` waits for both. The on-screen list starts in
  alphabetical order by host, owner, and repository. `s` cycles between that
  and most recent. Keep the JSON inventory in project-name order. The home list is
  one row per project and shows the host, then the repository owner parsed
  from the remote, not the branch. A heading names each visible column:
  host, owner, project, branch, and age. An open tree and an open project use
  a ruled frame; the home list stays a table. `t` toggles the home list between that row and a tree of
  host, then owner, then repository, and leaves a project detail to show the tree.
  Host, owner, and project each have their own colour, in the table and the tree.
  Enter, `o`, and Ctrl-I, and any non-empty `/` search, list that project's
  checkouts by branch. Ctrl-O and Esc step back. The `/` filter and `:` command line share readline
  editing (Ctrl-B/F, Ctrl-A/E, Ctrl-W, Ctrl-U, Ctrl-K, Ctrl-D, Alt word
  keys). Do not append or pop those strings directly. Outside those fields,
  arrows, Ctrl-Y, and Ctrl-E move one row, Ctrl-U and Ctrl-D move half a
  page, `gg` jumps to the top, and `G` jumps to the bottom. The cursor moves
  inside the window, and the list scrolls when it comes within `hq.scrolloff`
  rows of the top or bottom (default 10). `/` also accepts
  `user:` and `organisation:` (`org:`,
  `organization:`). GitHub owner kind is resolved off the input thread and
  cached at `$XDG_CONFIG_HOME/bonsai/owners.json`; until it is known, or for
  any other host, either prefix matches the owner text. Do not block the list
  on that lookup. Paths stay on the status line. A missing checkout is marked
  `missing` and is not passed to git. Every hq warning is appended to
  `~/.local/state/bonsai/hq.log` (`XDG_STATE_HOME` overrides the directory,
  `_BONSAI_HQ_LOG` overrides the file, and `0` disables it).
- Measure `bonsai hq` footprints off the input thread, after the list is on
  screen. `shared` means a dependency directory is a symlink, contains
  hardlinked files, or lives in an external store such as Go modules.
  Copy-on-write and clonefile clones report their full logical size as `local`.
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
