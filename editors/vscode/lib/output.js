"use strict";

function json(stdout, description) {
  try {
    return JSON.parse(stdout);
  } catch {
    throw new Error(`Bonsai returned invalid ${description} JSON.`);
  }
}

function addPath(stdout) {
  const lines = stdout.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
  const path = lines.at(-1);
  if (!path) {
    throw new Error("Bonsai did not return a worktree path.");
  }
  return path;
}

function worktrees(stdout) {
  const value = json(stdout, "worktree list");
  if (!Array.isArray(value) || value.some((entry) => typeof entry?.path !== "string")) {
    throw new Error("Bonsai returned an unexpected worktree list.");
  }
  return value;
}

function cleanReport(stdout) {
  const value = json(stdout, "clean report");
  for (const key of ["planned", "skipped_dirty", "removed"]) {
    if (!Array.isArray(value?.[key])) {
      throw new Error("Bonsai returned an unexpected clean report.");
    }
  }
  return value;
}

/// Group list --all entries by their repo identifier, sorted for a stable
/// tree. Entries without one (unexpected layouts) land under "(unknown)".
function groupByRepo(entries) {
  const groups = new Map();
  for (const entry of entries) {
    const key = entry.repo ?? "(unknown)";
    if (!groups.has(key)) {
      groups.set(key, []);
    }
    groups.get(key).push(entry);
  }
  return [...groups.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([repo, list]) => ({
      repo,
      entries: [...list].sort((a, b) => (a.branch ?? "").localeCompare(b.branch ?? "")),
    }));
}

module.exports = { addPath, cleanReport, groupByRepo, worktrees };
