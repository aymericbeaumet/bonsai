# Workflow reliability and performance audit

Audited commit: `e536bf8752b8a4181f16fb018da4f4d722a2d68d` (`origin/main`).
Worktree branch: `ab/workflow-audit`. Date: 2026-09-07.

The existing feature set is sufficient for a daily workflow cornerstone. The
highest-value work is making ownership, deletion, shell composition, editor
state, and session discovery dependable. Fix the data-loss paths first, then
make frequent commands faster and failures easier to recover from.

This document records the original audit, before implementation. See the
[implementation results](2026-09-07-implementation.md) for subsequent changes
and verification. Recommendations below stay within the
existing commands, supported shells, providers, and editor workspace format.

## Verification

- `cargo test --locked`: 120 tests passed across two suites.
- `cargo clippy --locked --all-targets -- -D warnings`: passed.
- `cargo fmt --check`: passed.
- `cargo build --release --locked`: passed.
- Additional disposable fixtures exercised lifecycle hazards, session-store
  compatibility, Bash command substitution, closed stdout, and a real PTY.
- Verification ran locally on macOS arm64. Fish was unavailable; Windows and
  Linux behavior was inspected but not executed locally. Passing existing tests
  does not cover the reproduced failures below.

## 1. Protect work and preserve identity

### P1: A missing upstream is not evidence that work was merged

Evidence: `src/commands/clean.rs:42`, `:269`. Classification accepts
`upstream_gone` independently of merge or patch-equivalence evidence, then
cleanup force-deletes the branch.

Confirmed: a clean branch containing a committed file absent from main, with a
missing configured upstream, appears in `clean --dry-run --no-fetch --json` as
planned for removal. The destructive continuation follows directly from the
code; it was unnecessary to execute it. A remote branch can disappear without
being merged, or local commits can follow its deletion.

Recommendation: treat upstream disappearance as a reason to investigate. Require
positive integration evidence for the current branch tip before ordinary clean
can delete it. Preserve uncertain branches. Replace the test that currently
accepts ahead-plus-gone with cases covering unmerged deletion, squash merge,
post-push commits, and different configured remotes.

Also revalidate branch OIDs and cleanliness after confirmation. Another shell
can commit while the picker is open; a final dirty check does not protect newly
committed work. Coordinate Bonsai mutations with a repository lock, and use
expected-OID checks for ref changes because unrelated Git clients do not honor
that lock.

### P1: Enforce filesystem ownership consistently

Evidence: `src/commands/prune.rs:119`, `:133`;
`src/worktree.rs:183`; `src/commands/add.rs:70`.

Confirmed in disposable fixtures:

- A symlink below the configured root pointed outside it. Plain `prune --all`,
  without `--yes`, deleted an external `editor.code-workspace` and its now-empty
  parent directory. The empty-directory sweep follows symlinks and treats every
  file with that extension as disposable. Workspace generation was disabled.
- Orphan discovery followed the same kind of symlink and proposed an external
  directory for deletion.
- A symlink inside a project's branch directory redirected `add ab/escape/new`
  outside the root. Explicit `--path` validates canonical containment; the
  default path does not.

Recommendation: use non-following traversal, validate existing ancestors for
both default and explicit destinations, and revalidate before mutation. Delete
only known generated artifacts with verified ownership, not arbitrary workspace
files. Distinguish missing paths from unreadable or temporarily unavailable
paths. Test external links, cycles, dangling links, permission failures, and
concurrent path replacement.

### P2: Preserve exact existing branch names

Evidence: `src/commands/add.rs:35` normalizes before resolving existing refs.

Confirmed: `add ab/Existing_CASE` created a different `ab/existing-case` branch
from main even though the requested branch existed. Picker suggestions can hit
the same path.

Recommendation: resolve exact existing local and remote refs first. Apply
task-title slugification only when creating a new branch. Cover case,
underscores, dots, and Unicode; make normalization visible when creating.

### P2: Recover coherently after partial removal

Evidence: `src/commands/remove.rs:54` and `:66`;
`src/commands/clean.rs:266`; `src/main.rs:25`.

Confirmed: removing the current unmerged worktree with `-d` deleted its directory,
then branch deletion failed. Exit status was 1, but no return-home sentinel was
emitted. The branch survived; the parent shell was stranded in a deleted cwd.

Recommendation: preflight predictable failures, remove the current directory
last, and carry an explicit operation outcome containing completed work,
remaining failures, exit status, and any required cwd recovery. Emit recovery
even on partial failure and keep machine-readable results available. Test
failure at each transition, including multi-target commands.

## 2. Make the existing integrations trustworthy

### P1: Preserve editor-owned workspace content

Evidence: `src/workspace.rs:68`, `:137`, and deletion at `:40`/`:91`.
Both workspace writers replace the entire document with `folders` only.
Editor settings, tasks, debug configuration, extensions, and comments are lost;
removing the last worktree can delete customizations outright.

Recommendation: update Bonsai-owned folder entries with JSONC-aware editing,
preserve other content, and retain customized files when the folder inventory
empties. Use a per-file lock spanning rescan/read/merge plus atomic replacement.
Atomic replacement alone cannot prevent a stale snapshot overwriting a newer
one. Skip writes when content is unchanged to avoid needless editor reloads.

The overwrite is source-confirmed; concurrent stale/torn writes are an inferred
race, not a reproduced result. Add preservation, concurrent-update, interrupted
write, and no-op-write tests. The format supports comments and workspace-owned
configuration: [VS Code multi-root workspace documentation](https://code.visualstudio.com/docs/editing/workspaces/multi-root-workspaces).

### P1/P2: Make shell composition work after initialization

Evidence: `src/shell.rs:114`. Confirmed in Bash: after loading the generated
wrapper, `captured=$(bonsai cd ab/workflow-audit)` succeeds with empty output.
The wrapper consumes the path and changes directory in the substitution's
subshell. This also undermines the documented `path=$(bonsai add ...)` contract.

Recommendation: explicitly distinguish interactive auto-cd from captured machine
output. Preserve the raw path when called through command substitution; document
`command bonsai` as the immediate scripting escape hatch. Forward commands that
cannot change cwd directly, avoiding whole-output buffering. Keep ordinary shell
`cd` so users' existing directory-change hooks can run.

Test executable wrappers in Bash, Zsh, and Fish: direct calls, substitutions,
pipes, global options, spaces in paths, failed cd, strict options, and cwd hooks.
Fish currently returns the earlier binary status after `cd` (`src/shell.rs:164`),
so a failed cd can report success; this is source-derived, not locally executed.
Zsh option isolation can use the documented
[`emulate -L zsh`](https://zsh.sourceforge.io/Doc/Release/Options.html).

### P2: Restore terminal state and handle closed pipes

Evidence: `src/picker.rs:62`; printing in `src/main.rs:42`.

Confirmed in an isolated PTY: Escape restores terminal settings, but SIGTERM
after the Worktree picker appears leaves them altered. Confirmed separately:
closing stdout before consuming `init bash` output causes a debug panic with
exit 101. The release profile uses `panic = "abort"`.

Recommendation: coordinate SIGTERM/SIGHUP cancellation through the terminal
cleanup path, and use fallible output with deliberate BrokenPipe handling. Do
not promise restoration after SIGKILL. Use terminal display width for CJK,
combining marks, and emoji instead of character counts (`src/picker.rs:101`).
Test cancellation, resize, narrow terminals, redirected streams, and restoration
with real PTYs. Rust documents stdout-error panics for
[`println!`](https://doc.rust-lang.org/std/macro.println.html).

### P2: Reconcile session stores without reviving or losing sessions

Evidence: `src/commands/resume.rs:446`, `:458`, `:536`, `:565`, `:653`.

Confirmed with synthetic provider stores:

- A session archived in the newest database was launched when an older database
  or legacy transcript still represented it as active. Filtering archived rows
  before reconciliation discards the exclusion information.
- One unreadable legacy rollout discarded healthy database results for the
  provider. One malformed database timestamp likewise discarded valid rows.

Recommendation: retain authoritative per-session exclusion state from the newest
applicable store and use older sources to enrich genuinely absent IDs. Preserve
the existing fallback where an empty new database does not hide older sessions.
Isolate errors per record/source, preserve good results, and summarize partial
failures. Pin compatibility fixtures for schema migration, malformed records,
locked databases, missing files, and deleted worktree directories.

### P2: Provision with the intended tools and report what happened

Evidence: `src/pm.rs:53`, `:98`, `:175`, `:368`;
`src/commands/add.rs:147`, `:392`, `:452`.

The current manager detection accepts `packageManager` without requiring its
matching lockfile, despite the documented lockfile gate. Tool resolution uses
the caller's PATH before spawning in the destination; installations happen
before the example `post_add = "mise install"` hook. A directory change inside
the child does not rerun the parent shell's tool activation hooks.

Recommendations:

- Require the selected manager's lockfile. Distinguish a version declaration
  from the actual executable/version being run. Respect installed shims and
  document the existing hook as the explicit toolchain setup path when automatic
  installation is disabled. For example, a project's existing hook can use
  `mise install && mise exec -- <manager> <frozen-install-arguments>`; no new
  tool-manager subsystem is needed. [mise exec documentation](https://mise.jdx.dev/cli/exec.html).
- Surface missing tools and incomplete provisioning in a concise final summary.
  A failed install should remain non-destructive, but a returned path should not
  imply the environment is ready. Provide a concrete retry command; re-adding
  the existing worktree currently returns before provisioning.
- Consider `uv sync --locked` to check manifest/lock consistency. Confirmed with
  local uv 0.11.15: after adding a dependency absent from a generated lockfile,
  `uv sync --frozen --offline` returned 0, whereas `uv lock --check --offline`
  failed. `--frozen` intentionally skips freshness checks; see
  [uv locking and syncing](https://docs.astral.sh/uv/concepts/projects/sync/).
- Preserve current shared-store advice, but make warnings reflect effective
  configuration and supported versions. For example, Bun supports an environment
  override that the current file-only warning misses. Avoid declaring a setup
  slow solely because a local setting is absent. Current guidance is supported
  by [pnpm's worktree guide](https://pnpm.io/git-worktrees) and
  [Bun's global virtual store documentation](https://bun.sh/docs/pm/global-store).
  Do not change users' dependency linker semantics automatically.

Also reject invalid Git boolean values and protection globs rather than silently
falling back (`src/config.rs:197`, `src/commands/clean.rs:147`). This is especially
important when a typo silently re-enables installation or drops a protected rule.

## 3. Optimize work performed, then tune concurrency

### Remove the quadratic session join

`src/commands/resume.rs:633` scans every history ID for every rollout filename:
O(files × IDs). This runs even with a healthy database and an exact requested ID.

Synthetic release measurements, three trials each, successfully resumed the
same exact session from a healthy database:

| History IDs × unmatched rollouts | Median | Samples |
| --- | ---: | --- |
| 0 × 0 | 386.9 ms | 415.3, 262.0, 386.9 ms |
| 1,000 × 1,000 | 299.7 ms | 299.7, 284.6, 509.5 ms |
| 5,000 × 5,000 | 904.3 ms | 1,004.5, 904.3, 863.5 ms |
| 10,000 × 10,000 | 2,772.2 ms | 2,727.5, 3,031.8, 2,772.2 ms |

These include startup and repository discovery. Baseline variance makes small
differences inconclusive; the larger fixtures substantiate the quadratic join.
These are diagnostic measurements, not production performance targets.

Extract the structured session ID and hash-join in O(files + IDs), with explicit
compatibility handling for older filename formats. Resolve exact IDs using the
authoritative store before broad discovery. Defer transcript enrichment until
needed. Start with per-invocation indexes; add persistent caches only if measured
I/O still warrants their invalidation complexity.

### Make exact navigation and list cheaper

Release binary timings in the audit checkout with three registered worktrees,
ten sequential runs per command:

| Command | Median | Observed range |
| --- | ---: | ---: |
| `--version` | 3.0 ms | 2.7–715.3 ms |
| `init zsh` | 2.9 ms | 2.7–3.0 ms |
| `list --json` | 255.3 ms | 223.7–288.4 ms |
| `cd ab/workflow-audit` | 173.9 ms | 154.6–199.5 ms |

The first version invocation was a cold-start outlier. These are local wall-clock
samples, not portable performance guarantees or CI thresholds.

Repository discovery is repeated in `src/main.rs:70`, command entry points, and
inventory helpers. Build one invocation context with config, repository,
remote, and worktree inventory; reuse it until a mutation requires refresh.
Resolve exact branches before fetching activity and sorting all candidates
(`src/commands/cd.rs:20`). Preserve system Git compatibility rather than replacing
it with a second Git implementation purely to avoid subprocess startup.

`list --status` currently reports Git errors as clean
(`src/commands/list.rs:125`). Represent unknown status explicitly and identify the
affected checkout. Performance optimizations must never convert missing evidence
into a clean/safe result.

### Keep output live and resource use bounded

Installers use `Command::output()` and emit logs only after all parallel jobs
finish (`src/commands/add.rs:420`); hook stdout is also buffered. Stream labelled
progress with bounded buffering, preserve clean machine stdout, and propagate
cancellation to child processes. Maintain deterministic final summaries.

The existing eight-worker bound applies to each parallel map independently.
Nested resume scans can exceed it. Use a shared concurrency budget if profiling
shows contention; do not simply increase thread counts. Keep shared Git metadata
mutation sequential. Avoid rescanning and rewriting the entire global workspace
when a no-op or unchanged inventory can be established safely.

## Delivery order and acceptance criteria

| Batch | Existing behavior to strengthen | Acceptance |
| --- | --- | --- |
| 1 | Cleanup, ownership, branch identity | Unique commits never cleaned; no traversal/deletion outside ownership; exact refs preserved; partial failures recover cwd. |
| 2 | Shell, terminal, editor, provisioning | Substitution returns paths; advertised shells execute in CI; PTY restores on supported cancellation; editor content survives refresh; incomplete installs are explicit. |
| 3 | Resume correctness and latency | Archived sessions stay excluded; bad records do not hide good ones; exact-ID lookup avoids global transcript scans; scaling join is linear. |
| 4 | Common-path overhead and regression gates | Shared invocation context; exact-cd fast path; no-op workspace writes avoided; bounded child logs/concurrency. |

Use failing contract tests for each reproduced issue before implementing it.
Explicitly install advertised shells in at least one CI job: current tests skip
missing shells (`tests/cli.rs:1286`), and `.github/workflows/ci.yml` does not
guarantee their presence. Extend the existing six-platform matrix with focused
PTY and real package-manager smoke coverage rather than relying only on argument
stubs.

Benchmark release builds on fixed fixtures with 1/10/100 worktrees and
1k/10k/100k session records, cold and warm. Record median/p95, child-process count,
bytes read, and peak memory; include slow/missing files and simultaneous
commands. A reasonable proposed warm exact-navigation target is below 100 ms
on a defined local reference machine. Set thresholds after collecting stable
baselines; no current measurement establishes a cross-platform p95.

No additional commands, providers, editor extensions, terminal daemons, or
background services are required for these improvements.
