"use strict";

const { execFile } = require("node:child_process");
const path = require("node:path");
const { promisify } = require("node:util");
const vscode = require("vscode");
const os = require("node:os");
const { addPath, cleanReport, groupByRepo, worktrees } = require("./lib/output");

const execFileAsync = promisify(execFile);
const MAX_OUTPUT_BYTES = 10 * 1024 * 1024;

/** @type {import("vscode").OutputChannel} */
let output;
/** @type {WorktreeTreeProvider} */
let tree;

function configuration() {
  return vscode.workspace.getConfiguration("bonsai");
}

async function workspaceDirectory() {
  const active = vscode.window.activeTextEditor?.document.uri;
  const activeFolder = active && vscode.workspace.getWorkspaceFolder(active);
  if (activeFolder) {
    return activeFolder.uri.fsPath;
  }

  const folders = vscode.workspace.workspaceFolders ?? [];
  if (folders.length === 1) {
    return folders[0].uri.fsPath;
  }
  if (folders.length > 1) {
    const picked = await vscode.window.showQuickPick(
      folders.map((folder) => ({
        label: folder.name,
        description: folder.uri.fsPath,
        folder,
      })),
      { placeHolder: "Choose the repository for this Bonsai command" },
    );
    return picked?.folder.uri.fsPath;
  }

  throw new Error("Open a Git repository folder before running a Bonsai command.");
}

async function runBonsai(args, cwd) {
  const executable = configuration().get("executable", "bonsai");
  const env = { ...process.env };
  delete env._BONSAI_WRAPPED;

  output.appendLine(`> ${executable} ${args.join(" ")}`);
  try {
    const result = await execFileAsync(executable, args, {
      cwd,
      env,
      windowsHide: true,
      maxBuffer: MAX_OUTPUT_BYTES,
    });
    if (result.stdout.trim()) {
      output.appendLine(result.stdout.trimEnd());
    }
    if (result.stderr.trim()) {
      output.appendLine(result.stderr.trimEnd());
    }
    return result;
  } catch (error) {
    const stderr = typeof error.stderr === "string" ? error.stderr.trim() : "";
    const detail = error.code === "ENOENT"
      ? `Cannot find '${executable}'. Install Bonsai or set bonsai.executable.`
      : stderr || error.message;
    output.appendLine(detail);
    throw new Error(detail);
  }
}

async function openFolder(path) {
  const forceNewWindow = configuration().get("openWorktreesInNewWindow", true);
  await vscode.commands.executeCommand(
    "vscode.openFolder",
    vscode.Uri.file(path),
    { forceNewWindow },
  );
}

async function listWorktrees(cwd) {
  const result = await runBonsai(["list", "--json", "--status"], cwd);
  return worktrees(result.stdout);
}

function worktreeItems(entries) {
  return entries.map((entry) => {
    const flags = [];
    if (entry.main) flags.push("main checkout");
    if (entry.dirty) flags.push("dirty");
    if (entry.locked) flags.push("locked");
    if (entry.prunable) flags.push("prunable");
    return {
      label: entry.branch ?? "(detached)",
      description: flags.join(" · "),
      detail: entry.path,
      entry,
    };
  });
}

async function createWorktree() {
  const cwd = await workspaceDirectory();
  if (!cwd) return;
  const branch = await vscode.window.showInputBox({
    title: "Create a Bonsai worktree",
    prompt: "Branch name",
    placeHolder: "feat/my-change",
    ignoreFocusOut: true,
    validateInput: (value) => value.trim() ? undefined : "Enter a branch name.",
  });
  if (branch === undefined) return;

  const result = await vscode.window.withProgress(
    { location: vscode.ProgressLocation.Notification, title: `Creating ${branch.trim()}…` },
    () => runBonsai(["add", branch.trim()], cwd),
  );
  tree.refresh();
  await openFolder(addPath(result.stdout));
}

async function openWorktree() {
  const cwd = await workspaceDirectory();
  if (!cwd) return;
  const entries = await listWorktrees(cwd);
  const picked = await vscode.window.showQuickPick(worktreeItems(entries), {
    title: "Open a Bonsai worktree",
    matchOnDescription: true,
    matchOnDetail: true,
  });
  if (picked) {
    await openFolder(picked.entry.path);
  }
}

async function openRepositoryWorkspace() {
  const cwd = await workspaceDirectory();
  if (!cwd) return;
  const result = await runBonsai(["workspace"], cwd);
  await openFolder(addPath(result.stdout));
}

async function removeWorktree() {
  const cwd = await workspaceDirectory();
  if (!cwd) return;
  const entries = await listWorktrees(cwd);
  const removable = entries.filter(
    (entry) => !entry.main && entry.branch,
  );
  if (removable.length === 0) {
    await vscode.window.showInformationMessage("Bonsai has no removable worktrees in this repository.");
    return;
  }

  const picked = await vscode.window.showQuickPick(worktreeItems(removable), {
    title: "Remove a Bonsai worktree",
    placeHolder: "The branch is kept; dirty worktrees are refused.",
    matchOnDescription: true,
    matchOnDetail: true,
  });
  if (!picked) return;
  const confirmation = await vscode.window.showWarningMessage(
    `Remove the '${picked.entry.branch}' worktree? The branch will be kept.`,
    { modal: true },
    "Remove Worktree",
  );
  if (confirmation !== "Remove Worktree") return;

  await runBonsai(["remove", picked.entry.branch], cwd);
  if (path.resolve(cwd) === path.resolve(picked.entry.path)) {
    const main = entries.find((entry) => entry.main);
    if (main) {
      await openFolder(main.path);
    }
  }
  tree.refresh();
  await vscode.window.showInformationMessage(`Removed Bonsai worktree '${picked.entry.branch}'.`);
}

async function cleanWorktrees() {
  const cwd = await workspaceDirectory();
  if (!cwd) return;
  const preview = await vscode.window.withProgress(
    { location: vscode.ProgressLocation.Notification, title: "Inspecting merged Bonsai worktrees…" },
    async () => cleanReport((await runBonsai(["clean", "--dry-run", "--json"], cwd)).stdout),
  );
  if (preview.planned.length === 0) {
    const skipped = preview.skipped_dirty.length;
    await vscode.window.showInformationMessage(
      skipped
        ? `No clean worktrees to remove; ${skipped} dirty worktree(s) were skipped.`
        : "Bonsai found no merged worktrees to clean.",
    );
    return;
  }

  const branches = preview.planned.map((entry) => entry.branch).join(", ");
  const confirmation = await vscode.window.showWarningMessage(
    `Delete ${preview.planned.length} worktree(s) and branch(es): ${branches}?`,
    { modal: true, detail: "Bonsai already performed a dry run. Dirty worktrees are never removed." },
    "Clean Worktrees",
  );
  if (confirmation !== "Clean Worktrees") return;

  const result = cleanReport(
    (await runBonsai(["clean", "--yes", "--no-fetch", "--json"], cwd)).stdout,
  );
  tree.refresh();
  await vscode.window.showInformationMessage(
    `Bonsai removed ${result.removed.length} worktree(s).`,
  );
}

/**
 * "Bonsai Worktrees" Explorer view: every worktree across every repo,
 * grouped by repo-id. Clicking a worktree opens it; inline actions open in a
 * new window or remove it.
 */
class WorktreeTreeProvider {
  constructor() {
    this._emitter = new vscode.EventEmitter();
    this.onDidChangeTreeData = this._emitter.event;
  }

  refresh() {
    this._emitter.fire(undefined);
  }

  async getChildren(element) {
    if (element) {
      return element.kind === "repo"
        ? element.entries.map((entry) => ({ kind: "worktree", entry }))
        : [];
    }
    // `list --all` works from anywhere; prefer a workspace folder so
    // repo-local bonsai configuration applies.
    const cwd = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? os.homedir();
    try {
      const result = await runBonsai(["list", "--all", "--json"], cwd);
      return groupByRepo(worktrees(result.stdout)).map((group) => ({ kind: "repo", ...group }));
    } catch {
      return []; // details are in the output channel; the view shows its welcome text
    }
  }

  getTreeItem(element) {
    if (element.kind === "repo") {
      const item = new vscode.TreeItem(element.repo, vscode.TreeItemCollapsibleState.Expanded);
      item.contextValue = "repo";
      item.iconPath = new vscode.ThemeIcon("repo");
      return item;
    }
    const { entry } = element;
    const item = new vscode.TreeItem(
      entry.branch ?? "(detached)",
      vscode.TreeItemCollapsibleState.None,
    );
    item.contextValue = "worktree";
    item.iconPath = new vscode.ThemeIcon("git-branch");
    item.description = [
      entry.dirty && "dirty",
      entry.locked && "locked",
      entry.prunable && "prunable",
    ]
      .filter(Boolean)
      .join(" \u00b7 ");
    item.tooltip = entry.path;
    item.command = {
      command: "bonsai.openTreeItem",
      title: "Open Worktree",
      arguments: [element],
    };
    return item;
  }
}

async function openTreeItem(element) {
  if (element?.entry?.path) {
    await openFolder(element.entry.path);
  }
}

async function openTreeItemNewWindow(element) {
  if (element?.entry?.path) {
    await vscode.commands.executeCommand(
      "vscode.openFolder",
      vscode.Uri.file(element.entry.path),
      { forceNewWindow: true },
    );
  }
}

async function removeTreeItem(element) {
  const entry = element?.entry;
  if (!entry?.branch) return;
  const confirmation = await vscode.window.showWarningMessage(
    `Remove the '${entry.branch}' worktree? The branch will be kept.`,
    { modal: true },
    "Remove Worktree",
  );
  if (confirmation !== "Remove Worktree") return;
  // The worktree itself is a valid cwd for its own repo.
  await runBonsai(["remove", entry.branch], entry.path);
  tree.refresh();
  await vscode.window.showInformationMessage(`Removed Bonsai worktree '${entry.branch}'.`);
}

function register(context, command, handler) {
  context.subscriptions.push(vscode.commands.registerCommand(command, async (...args) => {
    try {
      await handler(...args);
    } catch (error) {
      output.show(true);
      await vscode.window.showErrorMessage(`Bonsai: ${error.message}`);
    }
  }));
}

function activate(context) {
  output = vscode.window.createOutputChannel("Bonsai", { log: true });
  context.subscriptions.push(output);
  tree = new WorktreeTreeProvider();
  context.subscriptions.push(
    vscode.window.registerTreeDataProvider("bonsaiWorktrees", tree),
  );
  register(context, "bonsai.refreshTree", () => tree.refresh());
  register(context, "bonsai.openTreeItem", openTreeItem);
  register(context, "bonsai.openTreeItemNewWindow", openTreeItemNewWindow);
  register(context, "bonsai.removeTreeItem", removeTreeItem);
  register(context, "bonsai.add", createWorktree);
  register(context, "bonsai.openWorkspace", openRepositoryWorkspace);
  register(context, "bonsai.open", openWorktree);
  register(context, "bonsai.remove", removeWorktree);
  register(context, "bonsai.clean", cleanWorktrees);
  register(context, "bonsai.showOutput", () => output.show());
}

function deactivate() {}

module.exports = { activate, deactivate };
