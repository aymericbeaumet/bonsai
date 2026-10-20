# How it works

- Worktrees live at `<root>/<repo-id>/<branch>`, where the repo-id is derived
  from the remote URL (`github.com/owner/repo`, nested groups preserved) or a
  hash of the repo path when there is no remote. Inputs are slugified per `/`
  segment, and the resulting branch maps to nested directories (`Fix API/Login`
  → branch and directory `fix-api/login`).
- Git-registered worktrees elsewhere under the configured global root remain
  managed, so moving the main checkout or changing its repo ID does not orphan
  them. Registered worktrees outside that root are external and merged into the
  project's read-only views; Bonsai never creates there.
- For interactive cd-capable commands, the shell wrapper watches for a sentinel
  line to cd. Other commands and captured output pass through directly. Prompts
  render on stderr, so fuzzy pickers work even inside `$(...)`.

See [Commands](commands.md#concurrency-and-safety) for the concurrency and
locking guarantees.

## Use as a Rust library

Worktree discovery and the commands behind the CLI are packaged as
`bonsai-lib`, which the CLI builds on and which is imported as `bonsai`.
Each release publishes it to [crates.io](https://crates.io/crates/bonsai-lib)
at the CLI's version:

```sh
cargo add bonsai-lib --rename bonsai
```

```rust
use bonsai::repo::repo_id_from_url;

let id = repo_id_from_url("https://github.com/aymericbeaumet/bonsai.git");
assert_eq!(id.as_deref(), Some("github.com/aymericbeaumet/bonsai"));
```

API documentation is on [docs.rs](https://docs.rs/bonsai-lib); in a clone,
run `cargo doc --open -p bonsai-lib`.
