import test from "node:test";
import assert from "node:assert/strict";
import {
  fuzzyScore,
  headquartersRows,
  worktreeSummary,
  agentHierarchy,
  type Agent,
  type WorkspaceState,
  parseArguments,
  filterProjects,
  layoutGraph,
  escapeHtml,
  terminalInputChunks,
  runtimeActivity,
  type Project,
  type Worktree,
} from "./model.ts";

test("large terminal pastes stay within frame limits and preserve multibyte input", () => {
  const input = new TextEncoder().encode("A branch 🌱\n".repeat(9000));
  const chunks = terminalInputChunks(input);
  assert.ok(chunks.length > 1);
  assert.ok(chunks.every((chunk) => chunk.length <= 16 * 1024));
  assert.deepEqual(new Uint8Array(Buffer.concat(chunks)), input);
  assert.deepEqual(terminalInputChunks(new Uint8Array()), []);
});

const tree = (path: string, branch: string): Worktree => ({
  path,
  branch,
  head: "0123456789",
  main: false,
  external: false,
  locked: false,
  prunable: false,
  dirty: false,
  added: 0,
  modified: 0,
  deleted: 0,
  untracked: 0,
  ahead: 0,
  behind: 0,
});
const projects: Project[] = [
  {
    id: "github.com/team/api",
    name: "api",
    path: "/api",
    remote: null,
    worktrees: [
      tree("/api/main", "main"),
      tree("/api/fix-login", "ab/fix-login"),
    ],
  },
  {
    id: "github.com/team/web",
    name: "web",
    path: "/web",
    remote: null,
    worktrees: [tree("/web/main", "main")],
  },
];

test("runtime activity groups every pane and excludes finished terminals without changing worktree membership", () => {
  const inactive = projects[0].worktrees[0];
  const active: Worktree = {
    ...projects[0].worktrees[1],
    activity: {
      terminals: [
        { id: "running", title: "Shell", kind: "shell", exited: false },
        { id: "finished", title: "List", kind: "command", exited: true },
      ],
      tmux: [
        {
          session: "coding",
          window: "@0",
          pane: "%0",
          command: "vim",
          active: true,
          windowName: "editor",
          windowIndex: 0,
        },
        {
          session: "coding",
          window: "@1",
          pane: "%1",
          command: "bash",
          active: false,
          windowName: "tests",
          windowIndex: 1,
        },
      ],
    },
  };
  const activity = runtimeActivity(active);
  assert.equal(activity.summary, "terminal 1 · tmux 2");
  assert.deepEqual(
    activity.terminals.map((terminal) => terminal.id),
    ["running"],
  );
  assert.equal(activity.sessions.length, 1);
  assert.equal(activity.sessions[0].panes.length, 2);
  assert.equal(runtimeActivity(inactive).summary, "");
  const annotated = [
    { ...projects[0], worktrees: [inactive, active] },
    projects[1],
  ];
  const coordinates = (input: Project[]) =>
    layoutGraph(input).groups.map((group) => ({
      id: group.project.id,
      x: group.x,
      y: group.y,
      worktrees: group.worktrees.map((node) => ({
        path: node.worktree.path,
        x: node.x,
        y: node.y,
      })),
    }));
  assert.deepEqual(coordinates(annotated), coordinates(projects));
  assert.equal(
    filterProjects(annotated, "", null).flatMap((project) => project.worktrees)
      .length,
    3,
  );
});

test("fuzzy matching prefers contiguous matches and respects character order", () => {
  assert.ok(
    fuzzyScore("login", "ab/login") >
      fuzzyScore("login", "long-other-great-item-name"),
  );
  assert.ok(fuzzyScore("afl", "ab/fix-login") >= 0);
  assert.equal(fuzzyScore("zz", "ab/fix-login"), -1);
  assert.equal(fuzzyScore("ba", "ab"), -1);
});

test("filter searches all projects and preserves project matches with every worktree", () => {
  assert.equal(filterProjects(projects, "api", null)[0].worktrees.length, 2);
  assert.equal(
    filterProjects(projects, "fix-login", null)[0].worktrees.length,
    1,
  );
  assert.equal(
    filterProjects(projects, "", "github.com/team/web")[0].name,
    "web",
  );
  assert.equal(filterProjects(projects, "missing", null).length, 0);
});

test("layout includes every worktree, remains finite empty, and avoids crowded project rows", () => {
  const large = {
    ...projects[0],
    worktrees: Array.from({ length: 60 }, (_, i) =>
      tree(`/api/${i}`, `ab/${i}`),
    ),
  };
  const layout = layoutGraph([large, projects[1], projects[0]]);
  assert.equal(layout.groups.flatMap((group) => group.worktrees).length, 63);
  assert.ok(
    layout.groups[2].worktrees[0].y > layout.groups[0].worktrees.at(-1)!.y + 80,
  );
  assert.ok(Number.isFinite(layoutGraph([]).width));
});

test("argument parsing handles grouped, empty, and escaped arguments without expansion", () => {
  assert.deepEqual(
    parseArguments("ab/feature --base \"origin/main\" --path 'my directory'"),
    ["ab/feature", "--base", "origin/main", "--path", "my directory"],
  );
  assert.deepEqual(parseArguments('"" one\\ two $HOME $(pwd)'), [
    "",
    "one two",
    "$HOME",
    "$(pwd)",
  ]);
  assert.deepEqual(parseArguments("'a\\b'"), ["a\\b"]);
  assert.throws(() => parseArguments('"open'), /quoted/);
  assert.throws(() => parseArguments("trailing\\"), /backslash/);
});

test("repository names and paths cannot inject markup", () => {
  assert.equal(
    escapeHtml('<img src=x onerror="x">'),
    "&lt;img src=x onerror=&quot;x&quot;&gt;",
  );
});

test("quoted paths preserve literal backslashes and POSIX escapes never expand input", () => {
  assert.deepEqual(parseArguments(String.raw`--path "C:\my folder\checkout"`), [
    "--path",
    String.raw`C:\my folder\checkout`,
  ]);
  assert.deepEqual(
    parseArguments(String.raw`"cost: \$5, quote: \"yes\", slash: \\"`),
    ['cost: $5, quote: "yes", slash: \\'],
  );
  assert.deepEqual(parseArguments("one\\\ntwo \\\n three"), [
    "onetwo",
    "three",
  ]);
  assert.deepEqual(parseArguments('"one\\\ntwo"'), ["onetwo"]);
});


test("headquarters prioritizes attention and activity, searches sessions, and has deterministic ties", () => {
  const state: WorkspaceState = { root: "/", projects: [{ ...projects[0], worktrees: [
    { ...tree("/old", "ab/old"), priority: "older" },
    { ...tree("/recent", "ab/recent"), priority: "recent", lastActivity: 500 },
    { ...tree("/busy", "ab/busy"), priority: "working", lastActivity: 100 },
    { ...tree("/input", "ab/input"), priority: "needs-you", lastActivity: 90 },
  ] }], warnings: [], tmux: { available: true, sessions: [] }, agents: [
    { id: "codex", provider: "codex", worktreePath: "/busy", title: "Investigate cache", model: "test-model", target: {tmuxSession: "dev", tmuxPane: "%22"} } as Agent,
  ] };
  assert.deepEqual(headquartersRows(state, "", null).map((row) => row.worktree.path), ["/input", "/busy", "/recent", "/old"]);
  assert.equal(headquartersRows(state, "%22", null)[0].worktree.path, "/busy");
  assert.equal(headquartersRows(state, "cache", null)[0].worktree.path, "/busy");
  assert.deepEqual(headquartersRows(state, "ab/old", null).map((row) => row.worktree.path), ["/old"]);
  assert.deepEqual(headquartersRows(state, "codex cache", null).map((row) => row.worktree.path), ["/busy"]);
  const reversed = { ...state, projects: state.projects.map((project) => ({ ...project, worktrees: [...project.worktrees].reverse() })) };
  assert.deepEqual(headquartersRows(state, "", null).map((row) => row.worktree.path), headquartersRows(reversed, "", null).map((row) => row.worktree.path));
});

test("agent hierarchy preserves independent sessions, children, orphans and cycles exactly once", () => {
  const agents = [
    {id: "b", parentId: null}, {id: "a", parentId: null}, {id: "child", parentId: "a"},
    {id: "orphan", parentId: "missing"}, {id: "cycle1", parentId: "cycle2"}, {id: "cycle2", parentId: "cycle1"},
  ] as Agent[];
  const rows = agentHierarchy(agents);
  assert.equal(rows.length, 6);
  assert.equal(new Set(rows.map((row) => row.agent.id)).size, 6);
  assert.equal(rows.find((row) => row.agent.id === "child")?.depth, 1);
  assert.equal(rows.find((row) => row.agent.id === "b")?.depth, 0);
});


test("collapsed worktrees summarize attention before newer live and saved sessions", () => {
  const worktree = tree("/api/main", "main");
  const agents = [
    { id: "saved", worktreePath: worktree.path, title: "Saved task", updatedAt: 900, live: false, state: "completed" },
    { id: "live", worktreePath: worktree.path, title: "Active task", updatedAt: 800, live: true, state: "running" },
    { id: "question", worktreePath: worktree.path, title: "Waiting task", updatedAt: 100, live: true, state: "waiting" },
  ] as Agent[];
  const state = { agents, attention: [] } as unknown as WorkspaceState;
  assert.equal(worktreeSummary(state, worktree)?.id, "question");
  agents[2].state = "completed"; agents[2].live = false;
  assert.equal(worktreeSummary(state, worktree)?.id, "live");
  agents[1].live = false;
  assert.equal(worktreeSummary(state, worktree)?.id, "saved");
});
