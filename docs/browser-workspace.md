# Headquarters architecture

`bonsai hq` runs a persistent local service with specialized terminal and
browser interfaces. The Ratatui TUI and HTTP server share inventory and the
same terminal manager. TypeScript, HTML, and CSS
live in `web/`; the Rust binary embeds the compiled files in `web/dist/`.
There is no development server, CDN, or Node.js process at runtime.
The default port is `47831`; `--port` overrides it, and `--port 0` asks the
operating system for an available port. A busy explicit or default port
produces an error so the address stays predictable.

## Terminal interface

The TUI starts automatically on interactive stdin/stdout; `--no-tui` keeps
the server headless. `--no-open` independently controls browser launching.
The TUI reads cached inventory on a background worker so Git status cannot
block keyboard input. Its commands launch the same Bonsai PTYs as browser
actions. Attaching a PTY uses the native terminal's rendering and raw input;
Ctrl+] detaches back to the TUI while the session stays available to either
interface. Headquarters shutdown restores the terminal and closes owned
shells; tmux sessions remain independent.

Home is a full-width hierarchy ordered Needs you, Working, Recent, Older. Older
is collapsed until expanded or searched. Each worktree and agent occupies one
row; details open on demand. Arrows or `j`/`k` navigate, Left/Right expand or
collapse, `/` searches, and `[`/`]` select attention items. Enter expands a
worktree or attaches its selected agent; `t` creates a shell. Tab switches
between Home, Terminals, and Integrations. Space opens contextual actions and
`?` shows shortcuts. Below the minimum size, unavailable actions are disabled.

Browser and TUI consume the same agent state and preserve selected identities,
filters, expansion, and scroll when returning from terminal attachment. Opening
a waiting agent never acknowledges its unresolved request. Completed results can
be marked reviewed independently of process liveness.

In the browser, `/` focuses search, `n` advances through attention, and Enter
opens the selected session's actions. The terminal workspace remains available
from its compact dock; the graph is an optional view.

## Inventory and graph

`src/web/inventory.rs` scans the configured root without descending into
checkouts. Canonical directory identities let it follow directory symlinks
without revisiting paths or entering cycles. It also reads folder references
from JSONC `.code-workspace` files, preserving their comments and custom
content, and includes the checkout where the server
started. Each discovered repository is expanded through Git's registered
worktrees, exposing its main checkout and external worktrees alongside
Bonsai-managed worktrees. The canonical common Git directory identifies a
project, so independent clones of the same remote remain distinct.

Graph edges describe project membership, not commit ancestry. Repositories
outside the root with no workspace reference are discoverable only when
starting Bonsai from that repository. Invalid worktree metadata appears as
unavailable, with warnings; failed status probes never report a clean tree.
Status uses Git porcelain v2 with NUL delimiters and optional index locking
disabled. Per-worktree probes use the existing bounded worker pool.

Worktrees exist independently of runtime activity. The API annotates each
checkout with related HQ terminals and tmux panes; absent or stopped runtimes
never remove a worktree from the inventory. Tmux discovery reads every pane
in every window on the current tmux server (the inherited socket when HQ
starts inside tmux, otherwise the default socket). Canonical pane directories,
including subdirectories and symlinks, map to the deepest containing worktree.
HQ terminal associations use the directory in which the terminal was opened.
The list and TUI prioritize observed agent activity and recent use; the optional
graph shows the same inventory. The terminal list preserves completed HQ command
output. Session history is shared with the resume command; HQ exact resumption
uses `bonsai resume --provider <tool> --session <id>` without fuzzy fallback.

## Local API

The server listens only on `127.0.0.1`. Each run creates a random token,
printed in the launch URL's fragment. The frontend moves it to tab-local
session storage and removes it from the address bar. REST calls use a
Bearer authorization header; WebSocket upgrades carry the token in their
query string. Requests validate the bound Host, and mutations and WebSocket
upgrades require the server's exact Origin. Assets are bundled and served
with a restrictive content security policy and no-referrer policy.

| Endpoint | Purpose |
|---|---|
| `GET /api/state` | Projects, ordered worktrees, agents, attention, quotas, integration health, runtime activity, discovery warnings, tmux sessions and panes |
| `POST /api/agents/{id}/actions` | Capability-checked reply, interrupt, approve, or reject with exact session/request identity |
| `POST /api/attention/{id}/acknowledge` | Mark a completed result reviewed |
| `POST /api/visits` | Record explicit use of a known worktree |
| `POST /api/integrations/{provider}/actions` | Install, repair, disable, or uninstall a tool integration |
| `GET /api/terminals` | Server-owned terminal tabs |
| `POST /api/terminals` | Open a shell, Bonsai command, or tmux client |
| `DELETE /api/terminals/{id}` | Close the owned PTY and child process |
| `GET /api/terminals/{id}/ws` | WebSocket terminal input, output, and resizing |

Terminal creation takes `path`, optional Bonsai `args`, optional existing
`tmux` session name and optional exact `tmuxPane`, or `newTmux: true`.
A pane must still belong to the selected session; missing targets fail without
substituting the currently active pane. The path must be a known checkout
or the configured root. An `add` command may also start from an explicitly
entered existing Git checkout, allowing the first project to be added from
the browser. Bonsai arguments are validated with the CLI parser;
the server supplies its root and remote and rejects nested `hq` commands.
Commands run as argv in the selected checkout, preserving its configuration,
`--base HEAD`, config-file copying, and interactive prompts.

WebSocket binary frames carry terminal bytes. Client text frames carry
`{"type":"resize","cols":120,"rows":30}`; server text frames report
`{"type":"exit","code":0}`. The frontend uses xterm.js and its fit addon.

## Terminal lifecycle

The server owns terminal sessions independently of WebSocket connections.
Closing or reloading a browser tab leaves its PTY alive, with bounded output
replay for reconnecting clients. Closing a terminal explicitly tears down its
PTY. Server shutdown closes its terminals. Tmux attachment owns only a tmux
client: detaching or stopping Bonsai leaves the underlying session alive.
Tmux is optional; ordinary shells require no tmux installation.

HQ queries and controls tmux through its
[native session commands](https://github.com/tmux/tmux/wiki/Getting-Started#creating-sessions).
Tmux owns shell
startup, new windows, and pane respawning, using the user's tmux and shell
configuration. No Bonsai shell helper or persistent startup files are needed
for tmux. Ordinary HQ shells use temporary startup files, owned by their
terminal session, to load Bonsai's shell integration.

## Agent adapters and attention

Provider adapters annotate inventory; they never replace it. Sessions carry
provider/runtime identity, parent identity, canonical worktree ownership, exact
terminal targets, capabilities, observation freshness, model, and task title.
Liveness is separate from running, waiting, idle, completed, failed, stopped,
and unknown states. A focused tmux pane or a process named `node` is not evidence
of agent state. Ambiguous process associations stay unresolved.

Claude's supported `agents --json --all` interface provides live session state.
Lifecycle hooks add child-agent events; a composed statusline command captures
model and subscription limits while forwarding the original command's output.
Codex uses its actual hosting app-server when reachable and lifecycle hooks for
additional observations. A standalone account probe is quota-only: its thread
state never describes separately hosted sessions. OpenCode uses a local plugin
and its runtime's native SDK for events and supported controls.

Automatic setup records ownership of its additions, preserves configuration
symlinks and existing hooks, and keeps installation separate from activation.
Codex requires native trust of new hooks; OpenCode plugins load at startup.
Repair/removal must preserve entries changed independently after installation.
The internal `__hq-event` bridge reads bounded event input and stores normalized
metadata, not transcripts, under `<root>/.hq`. Internal environment keys use
`_BONSAI_` so they cannot enter strict configuration parsing.

Attention includes outstanding questions/approvals, actionable errors, and new
completed results. Opening an agent does not resolve a request; provider events
are authoritative. Saved history does not generate completion notifications.
Recency combines explicit HQ use, provider activity, pane activity, and the
existing filesystem fallback. Polling timestamps never make a worktree recent.
Older means no qualifying activity in seven days and does not remove inventory.
Visits, native lifecycle transitions, and completed-result acknowledgements
persist under `<root>/.hq`; reconnecting preserves review identity. Failed native
probes retain their last observation until it becomes stale.

Quota observations include source, time, used percentage, and reset time. Unknown
allowance is never zero allowance, and an elapsed reset does not prove a refill.
Do not add together agents' observations of the same subscription. OpenCode has
no generic subscription quota endpoint; unsupported data remains unavailable.

`[hq] auto_setup = false` disables automatic installation. Desktop notifications
and tmux status integration are opt-in through `notifications` and `tmux_status`.
Desktop notifications use the native macOS or Linux notification command.
The latter appends a temporary count to the existing tmux status format and
removes its own suffix on clean shutdown. Neither option changes tmux shell
startup. All provider discovery and control work stays off the UI input thread.

## Development

```sh
cd web
npm ci
npm run check
npx playwright install chromium
cd ..
cargo build
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cd web
npm run test:e2e
cd ..
cargo run -- hq --no-open --port 0
```

Commit the frontend sources, package lock, and rebuilt `web/dist` together.
CI verifies TypeScript, frontend behavior, asset reproducibility, and browser
flows against the Rust server. Rust builds and release binaries use the
checked-in bundle.
Tests cover inventory parsing and discovery, HTTP authorization, PTY input,
resizing, replay, and terminal teardown. Playwright uses isolated repositories
and homes to exercise navigation, terminals, connection recovery, worktree
creation/removal, tmux when installed, onboarding, and narrow layouts. Set
`_BONSAI_BROWSER_CHANNEL=chrome` to use an installed Chrome instead of
Playwright's Chromium, or `_BONSAI_TEST_BIN` to test a different binary.
