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
  the CLI's dependency requirement in sync. The CLI is never published to
  crates.io (`publish = false`; the `bonsai-cli` name there belongs to another
  project): document Homebrew (`aymericbeaumet/tap/bonsai`), mise
  (`github:aymericbeaumet/bonsai`), and `cargo install --locked --git
  https://github.com/aymericbeaumet/bonsai`. Releases publish only
  `bonsai-lib` to crates.io (`CARGO_REGISTRY_TOKEN`) and regenerate the tap
  formula (`HOMEBREW_TAP_TOKEN`) through `.github/scripts/release.py`; keep
  its release archive names in sync with `release.yml`, and run
  `mise run release-helper-tests` after changing it. CLI sources and integration tests live in `bonsai-cli/`; library
  sources live in `bonsai-lib/`. The root package embeds
  `skills/bonsai/SKILL.md` so packaging does not reach outside the library
  crate.
- Keep `readme.md` a short landing page: pitch, demo, install, quick start,
  command summary, and links. Detailed reference belongs in `docs/*.md`;
  update the page that owns a behavior when changing it.
- `docs/demo/demo.gif` is recorded by `mise run demo` (VHS in a container,
  bonsai built from the checkout). Re-record it when the commands or output
  it shows change.
- mise is the single development environment: `.mise.toml` pins the tools
  and defines a task for every command contributors and CI run. Add a task
  rather than a Makefile, a shell alias, or a raw command in CI or the docs,
  and keep `docs/development.md` listing them.
- Before finishing, run `mise run check`: formatting, Clippy with warnings
  denied, tests, packaging, actionlint, and the release helper tests.
- Locally, `cargo package --workspace` may verify the CLI against a stale
  `bonsai-lib` because the placeholder version never changes: cargo reuses
  the unpacked copy in `~/.cargo/registry/src/*/bonsai-lib-0.1.1` and its
  build artifacts. If it reports errors the workspace build does not, delete
  that directory, run `cargo clean -p bonsai-lib`, and package again.
- Trust `.mise.toml` with `mise trust .mise.toml` and run
  `mise install --locked`. Tool pins and platform checksums live in
  `.mise.toml` and `mise.lock`. CI sets `MISE_TASK_RUN_AUTO_INSTALL=false`,
  so each job uses only the tools it installs.
- `bonsai hq` is a terminal worktree list that also runs commands. Inventory
  is Git checkouts under the bonsai root, workspace folder references, and
  the project where hq starts. Do not add a browser, a server, or a second
  UI, and do not run a shell or tmux inside hq. Bonsai commands run as child
  processes (`bonsai -C <project> …`) streaming into the output pane; coding
  agents and editors run in the foreground on `/dev/tty` while the interface
  is suspended. Removal, branch deletion, clean, trim, and prune ask inside hq first
  (clean shows its `--dry-run --json` plan, then runs `--yes --no-fetch`;
  trim shows its `--dry-run --json` plan, then runs `--yes` on the reviewed
  paths; prune previews exact missing registrations/orphan paths before `--yes`);
  never pass `--force`.
- HQ commands resolve the selected row once: host/owner groups, project, or
  exact checkout. Keep commands available in every view; only their targets
  change. Carry known project roots into child discovery, including projects
  whose linked checkouts are missing. Destructive commands show exact paths
  in an in-UI confirmation and run in the output pane; only explicit `o`
  opens full-screen output. Destructive command initials are uppercase
  (`R`, `D`, `C`, `P`, `T`); other command shortcuts match their names, with
  Ctrl-R/Ctrl-S for refresh/sort. Derive dispatch from `COMMANDS`, and wrap
  footer hints across two lines without splitting key-label pairs. Apply
  reviewed trim/prune manifests under locks
  so confirmation cannot widen from metadata removal to directory deletion.
- hq draws on stderr so the shell wrapper can capture stdout for the cd
  target (Enter on a checkout, or `c`). Never call `Terminal::clear`: it
  queries the cursor position on stdout. Clear with an escape sequence and
  `resize` to force a redraw.
- Spell every value out under a column heading; no bare symbols or counts.
  Views: the home list (one row per project with its totals), an open project
  (one row per checkout), and search results. The home list has four
  views, ordered from the shortest list to the longest so each adds one
  grouping level: table, projects (`host/owner/project` with checkouts),
  owners (`host/owner`, then projects), hosts (host, then owners). `v` moves
  forward, `V` back, both wrapping; tabs on the top border name them and
  mark the current one. Every tree group sums its checkouts. `table.rs` drops the least useful
  columns first when the terminal is narrow. Host, owner, and project keep
  their colours in the table and the tree; an open project or tree uses a
  ruled frame. Deleted checkouts sort last and are not counted as warnings.
- Keys follow vim: counts, `gg`/`G`, Ctrl-D/U/F/B, Ctrl-E/Y scroll without
  moving the cursor, `H`/`M`/`L`, `zz`/`zt`/`zb`, `{`/`}`, `h`/`l`. Parse
  them once with `keys::Keys` so every view takes the same motions: lists
  move the cursor, while help, dialogs, and the output view scroll through
  `keys::Pager`. Enter in `/` keeps the results for those keys. Every
  command has a key and a `:` name; keep `COMMANDS`, `view::HELP`, and
  `docs/hq.md` in sync (a test checks each command has its own help row).
  Give distinct actions separate mapping descriptions in help and footer
  hints; never combine them to save space. The
  `/` filter and `:` line edit through `LineInput`; do not append or pop
  their strings directly. `/` accepts `user:` and `organisation:` (`org:`,
  `organization:`); GitHub owner kind is resolved off the input thread and
  cached at `$XDG_CONFIG_HOME/bonsai/owners.json`, and until it is known
  either prefix matches the owner text. `running:` (or `running:<agent>`)
  keeps checkouts where coding agents run.
- Paint the list before git status; status, sizes, owner kinds, and running
  agents (rescanned every two seconds) stream in off the input thread. `--json` waits for status and sizes and keeps
  project-name order. The last size of each checkout is kept in
  `~/.local/state/bonsai/footprints.json` (`_BONSAI_HQ_FOOTPRINTS` overrides
  the file, `0` disables it) so totals show at once; a partial walk never
  lowers a remembered size, and after a command only the affected project is
  measured again. `shared` means a dependency directory is a symlink,
  contains hardlinked files, or lives in an external store such as Go
  modules; copy-on-write clones report their full logical size as `local`.
  Every hq warning is appended to `~/.local/state/bonsai/hq.log`
  (`XDG_STATE_HOME` overrides the directory, `_BONSAI_HQ_LOG` the file, and
  `0` disables it).
- `BONSAI_*` environment variables whose first segment names a setting are
  strict: unknown nested keys (`BONSAI_ADD__FETHC`) fail. Other `BONSAI_*`
  variables (e.g. Heroku's `BONSAI_URL`) are ignored. Prefix internal
  process/bootstrap/test variables with `_BONSAI_`.
- Repository discovery caches the initial worktree inventory only within one
  invocation. `Repo::lock_mutations` must invalidate it: an interactive wait or
  another Bonsai process may have changed the inventory before lock acquisition.
- Workspace folder ownership lives in `.locks/workspace-*.folders.json`, under
  the corresponding workspace lock. Preserve unknown entries; commit workspace
  content before ownership metadata so interruption cannot create false ownership.
- Cwd recovery belongs to operation outcomes, including partial failures.
  Unwrapped `clean --json` carries recovery inside its single JSON document;
  shell-wrapped navigation uses the sentinel protocol.
- Destructive commands share their safety decisions: `agents::busy` for
  worktrees another coding agent works in (`remove` refuses, `clean` and
  `trim` skip; the agent running bonsai is exempt), and `ignored::Keep` for
  ignored paths `trim` and removal never delete. Reuse them in new paths.
- Treat every Git-registered linked checkout as a first-class worktree,
  regardless of creator or location. `Repo::linked_worktrees` supplies
  lifecycle targets; validate registrations before destructive work. The
  `external` flag describes location only. Protect registered descendants
  when trimming or removing a parent checkout.
- Trim validates parents without resolving the final symlink: dangling
  package links must be unlinked safely. Keep progress counts separate from
  disk usage so deletion does not need a metadata lookup for every file.
- Give every subcommand exactly one `GROUPS` entry in `bonsai-lib/cli.rs`;
  the top-level help is generated from it. Bulk commands (`trim`, `clean`,
  `prune`) share `-n/--dry-run`, `-y/--yes` (alias `-f`), and `--json`.
- `REQUIRE_TEST_SHELLS=1` makes Bash/Zsh/Fish coverage mandatory.
- Release PTY checks are in `tests/terminal.py`; scalable disposable benchmarks
  are in `scripts/benchmark.py`. The two real package-manager CLI tests are
  ignored in ordinary runs and must be requested with `--ignored`.
