"use strict";

const assert = require("node:assert/strict");
const test = require("node:test");
const { addPath, cleanReport, groupByRepo, worktrees } = require("../lib/output");

test("addPath returns the last non-empty output line", () => {
  assert.equal(addPath("\n/tmp/worktree\n"), "/tmp/worktree");
});

test("worktrees parses Bonsai list JSON", () => {
  assert.deepEqual(
    worktrees('[{"branch":"feat-x","path":"/tmp/feat-x","main":false}]'),
    [{ branch: "feat-x", path: "/tmp/feat-x", main: false }],
  );
});

test("cleanReport requires every report collection", () => {
  const report = { dry_run: true, planned: [], skipped_dirty: [], removed: [] };
  assert.deepEqual(cleanReport(JSON.stringify(report)), report);
  assert.throws(() => cleanReport('{"planned":[]}'), /unexpected clean report/);
});

test("groupByRepo groups and sorts worktrees for the tree view", () => {
  const grouped = groupByRepo([
    { repo: "github.com/o/r", branch: "zeta", path: "/b/github.com/o/r/zeta" },
    { repo: "github.com/o/r", branch: "alpha", path: "/b/github.com/o/r/alpha" },
    { repo: "github.com/a/a", branch: "main2", path: "/b/github.com/a/a/main2" },
    { branch: "stray", path: "/b/stray" },
  ]);
  assert.deepEqual(
    grouped.map((group) => [group.repo, group.entries.map((entry) => entry.branch)]),
    [
      ["(unknown)", ["stray"]],
      ["github.com/a/a", ["main2"]],
      ["github.com/o/r", ["alpha", "zeta"]],
    ],
  );
});
