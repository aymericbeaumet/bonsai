# Bonsai Worktrees for VS Code

Create, open, remove, and safely clean [Bonsai](https://github.com/aymericbeaumet/bonsai)
Git worktrees without leaving VS Code.

## Requirements

Install the `bonsai` CLI first:

```sh
cargo install --git https://github.com/aymericbeaumet/bonsai bonsai-cli
```

The extension runs the CLI in the active workspace. Set `bonsai.executable`
if `bonsai` is not on VS Code's `PATH`.

## Worktrees view

The **Bonsai Worktrees** view in the Explorer sidebar lists every worktree of
every repository under the bonsai root, grouped by repo. Click a worktree to
open it; inline actions open it in a new window or remove it (branch kept).
The view refreshes after every Bonsai command, or manually via its refresh
button.

## Commands

- **Bonsai: Create Worktree** creates or reuses a branch worktree and opens it.
- **Bonsai: Open Repository Workspace** opens Bonsai's maintained multi-root
  workspace containing the main checkout and every worktree.
- **Bonsai: Open Worktree** lists the repository's worktrees with dirty-state hints.
- **Bonsai: Remove Worktree** removes a selected clean worktree and keeps its branch.
- **Bonsai: Clean Merged Worktrees** previews the cleanup, asks for confirmation,
  then removes merged worktrees and their branches. Dirty worktrees are skipped.
- **Bonsai: Show Output** opens the extension's command log.

New and selected worktrees open in a new window by default. Disable
`bonsai.openWorktreesInNewWindow` to reuse the current window.

## Build a VSIX

```sh
npm run check
npm run package
code --install-extension bonsai-0.1.0.vsix
```
