# Workflow audit implementation

Implementation of the [2026-09-07 audit](2026-09-07-workflow-audit.md), confined
to the existing commands, shells, session providers, and editor workspace format.

## Changes

- Cleanup requires positive integration evidence, not a missing upstream.
  Repository/root mutation locks, post-confirmation checks, and expected-OID
  ref deletion protect against concurrent changes. Removal preflights targets,
  handles the current directory last, and reports partial progress with cwd
  recovery. Clean JSON stays one document, including on partial failure.
- Destination containment covers default and explicit paths. Discovery and prune
  do not follow directory symlinks; unknown files and uncertain Git metadata
  survive. Lock files reject linked targets. Existing exact local/remote branch
  identities take precedence over normalization, including newly fetched refs.
- Each invocation reuses repository identity/configuration and its initial
  inventory. Acquiring a mutation lock invalidates that inventory before use.
  Exact navigation avoids recency scanning; exact authoritative session IDs
  avoid unrelated provider/history scans. Legacy discovery uses indexed joins.
- Newer session-store exclusions suppress stale copies. Corrupt or unreadable
  records do not discard healthy sessions; partial failures produce diagnostics.
  Unknown Git status is explicit (`dirty: null` when status was requested).
- Shell wrappers auto-cd only with terminal stdout and preserve substitution,
  pipelines, redirection, directory hooks, and failed-cd status. Provider sessions
  retain direct stdio. Broken pipes exit quietly. Unix pickers restore terminal
  modes on interruption, hangup, termination, and ordinary exit.
- Workspace refresh preserves JSONC comments, settings, tasks, custom folders,
  and custom labels. Per-file locks, atomic replacement, and editor-change
  checks protect updates. Ownership sidecars under `.locks` identify previously
  generated entries; unknown migration content is preserved conservatively.
- Dependency installation requires the selected manager's matching lockfile;
  uv uses `sync --locked`. Shared-store advisories account for relevant inherited
  configuration and environment overrides. Relative executable lookup uses the
  destination directory. Hooks/install output streams through bounded buffers
  with labels; failures explain incomplete provisioning and how to retry.
  Unix cancellation reaches the child process group, including descendants
  still holding output pipes after their parent exits.
- Help, README, bundled workflow guidance, and CI reflect these contracts.
  CI retains the platform matrix and adds required Bash/Zsh/Fish coverage,
  real npm/uv checks, release PTY tests, and a benchmark smoke run.

## Local verification

On macOS arm64:

- `cargo test --locked`: 178 passed; two real-tool tests intentionally ignored.
  Bash, Zsh, and Fish were all present with `REQUIRE_TEST_SHELLS=1`.
- `cargo test --locked --test cli real_package_managers_ -- --ignored`:
  two passed (npm lock preservation and uv stale-manifest rejection).
- `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`,
  `actionlint`, and `git diff --check`: passed.
- `cargo build --release --locked` and
  `python3 tests/terminal.py target/release/bonsai`: passed; five PTY tests.

Linux/Windows CI was configured but not executed locally. The real-tool smoke
tests do not constitute a complete version matrix for every package manager.

## Release measurements

Same checkout, ten warm samples per command:

| Command | Original median | Updated median |
| --- | ---: | ---: |
| `list --json` | 255.3 ms | 136.2 ms |
| `cd ab/workflow-audit` | 173.9 ms | 67.5 ms |

Sequential remeasurement of the original synthetic exact-resume fixture:

| History IDs and unrelated rollouts | Original median | Updated median |
| ---: | ---: | ---: |
| 1,000 each | 299.7 ms | 130.3 ms |
| 5,000 each | 904.3 ms | 114.1 ms |
| 10,000 each | 2,772.2 ms | 156.3 ms |

The largest original fixture improved approximately 17.7 times. Three samples
per cell; fixtures and command were unchanged between the audit and remeasurement.

The reproducible `scripts/benchmark.py` harness also covered larger inventories:

| Managed worktrees | Exact cd median | List median | List with status median |
| ---: | ---: | ---: | ---: |
| 1 | 58.4 ms | 101.5 ms | 123.4 ms |
| 10 | 57.2 ms | 94.9 ms | 159.1 ms |
| 100 | 80.2 ms | 138.6 ms | 499.4 ms |

Exact cd used three Git processes at every size; list used five. Requested
status still adds one Git process per worktree. Clean dry-run medians at those
sizes were 279.2, 254.3, and 608.5 ms respectively.

| Records in each session source | Exact resume median | Broad title search median |
| ---: | ---: | ---: |
| 1,000 | 104.9 ms | 126.7 ms |
| 10,000 | 100.3 ms | 192.8 ms |
| 100,000 | 108.7 ms | 975.6 ms |

Each session size includes database rows, legacy history IDs, and unmatched
rollout files. The synthetic provider was launched successfully in every case.
Three warm samples per cell; no OS caches were flushed. The script records
first/warm samples and untimed Git process counts, not memory or bytes read.
These are local observations, not portable latency guarantees. The checkout
measurements overlapped unrelated benchmark activity; the original session
fixture comparison above was run sequentially.

## Deliberate limits

No new commands, providers, persistent session cache, or background daemon were
added. Existing bounded concurrency remains: these profiles do not justify
increasing it globally. Further scheduler changes should follow resource
profiling, especially for status and broad discovery.

Other Git clients and editors do not honor Bonsai locks. Expected-OID checks and
filesystem/content revalidation narrow those races but are not a filesystem
transaction against arbitrary external actors. Unknown editor entries are kept
on first migration rather than guessed to be disposable. An interrupted
workspace/ownership pair can leave an extra unclaimed folder safely behind.

Tool-version activation remains the user's shell/shim or explicit hook's job;
Bonsai does not install a runtime manager. Cancellation leaves a created worktree
available for recovery; re-adding it remains idempotent and does not rerun setup.
