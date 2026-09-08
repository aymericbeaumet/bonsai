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

Use arrows or `j`/`k` to navigate, `/` to fuzzy-find, and Enter or `t` to
attach a shell. Tab switches between worktrees and sessions. `e` attaches
existing activity in the selected worktree. Space opens the selected item's
actions; `:` runs a Bonsai command. Shortcuts include
`s` for start, `r` for resume, `a` for add, `m` for tmux, `b` for the browser,
and `q` to quit headquarters. Removal and closing sessions require a
confirmation. Completed command output stays visible until Enter or Ctrl+].

## Inventory and graph

`src/web/inventory.rs` scans the configured root without descending into
checkouts. Canonical directory identities let it follow directory symlinks
without revisiting paths or entering cycles. It also reads folder references
from `.code-workspace` files and includes the checkout where the server
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
The graph, list, and TUI show live activity; the inspector offers attachment
and preserves access to completed HQ command output through the terminal list.

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
| `GET /api/state` | Projects, worktrees, status, runtime activity, discovery warnings, tmux sessions and panes |
| `GET /api/terminals` | Server-owned terminal tabs |
| `POST /api/terminals` | Open a shell, Bonsai command, or tmux client |
| `DELETE /api/terminals/{id}` | Close the owned PTY and child process |
| `GET /api/terminals/{id}/ws` | WebSocket terminal input, output, and resizing |

Terminal creation takes `path`, optional Bonsai `args`, optional existing
`tmux` session name, or `newTmux: true`. The path must be a known checkout
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
