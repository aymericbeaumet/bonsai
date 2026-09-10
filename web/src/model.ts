export type Provider = "claude" | "codex" | "opencode" | "unknown";
export type Priority = "needs-you" | "working" | "recent" | "older";
export interface Agent {
  id: string;
  provider: Provider;
  sessionId: string | null;
  parentId: string | null;
  worktreePath: string | null;
  cwd: string;
  title: string;
  model: string | null;
  state: "unknown" | "running" | "waiting" | "idle" | "completed" | "failed" | "stopped";
  waitingReason: string | null;
  updatedAt: number | null;
  observedAt: number;
  live: boolean;
  stale: boolean;
  target: { terminalId?: string; tmuxSession?: string; tmuxPane?: string } | null;
  capabilities: string[];
}
export interface Attention {
  requestId?: string;
  id: string;
  agentId: string;
  kind: "question" | "approval" | "error" | "completed";
  summary: string;
  createdAt: number;
}
export interface Quota {
  id: string;
  provider: Provider;
  accountLabel?: string;
  label: string;
  usedPercent: number | null;
  resetsAt: number | null;
  observedAt: number;
  stale: boolean;
  unavailableReason?: string;
}
export interface Integration {
  provider: Provider;
  status: "unavailable" | "installed" | "awaiting-activation" | "connected" | "disabled" | "error";
  message: string;
  capabilities: string[];
}

export interface Worktree {
  path: string;
  branch: string | null;
  head: string;
  main: boolean;
  external: boolean;
  locked: boolean;
  prunable: boolean;
  dirty: boolean | null;
  added: number;
  modified: number;
  deleted: number;
  untracked: number;
  ahead: number;
  behind: number;
  lastActivity?: number | null;
  priority?: Priority;
  agentIds?: string[];
  activity?: {
    terminals: {
      id: string;
      title: string;
      kind: Session["kind"];
      exited: boolean;
    }[];
    tmux: TmuxActivity[];
  };
}

export interface TmuxActivity {
  session: string;
  window: string;
  pane: string;
  command: string;
  active: boolean;
  windowName: string;
  windowIndex: number;
}

export function runtimeActivity(worktree: Worktree) {
  const terminals = (worktree.activity?.terminals || []).filter(
    (terminal) => !terminal.exited,
  );
  const panes = worktree.activity?.tmux || [];
  const groups = new Map<string, TmuxActivity[]>();
  for (const pane of panes)
    groups.set(pane.session, [...(groups.get(pane.session) || []), pane]);
  const labels = [
    ...(terminals.length
      ? [
          {
            kind: "terminal",
            count: terminals.length,
            text: `terminal ${terminals.length}`,
          },
        ]
      : []),
    ...(panes.length
      ? [{ kind: "tmux", count: panes.length, text: `tmux ${panes.length}` }]
      : []),
  ];
  return {
    terminals,
    panes,
    sessions: [...groups].map(([session, sessionPanes]) => ({
      session,
      panes: sessionPanes,
    })),
    labels,
    summary: labels.map((label) => label.text).join(" · "),
  };
}

export interface Project {
  id: string;
  name: string;
  path: string;
  remote: string | null;
  worktrees: Worktree[];
}

export interface WorkspaceState {
  agents?: Agent[];
  attention?: Attention[];
  quotas?: Quota[];
  integrations?: Integration[];
  root: string;
  projects: Project[];
  warnings: string[];
  tmux: {
    available: boolean;
    sessions: {
      name: string;
      windows: number;
      attached: boolean;
      path: string;
    }[];
  };
}

export interface Session {
  id: string;
  title: string;
  path: string;
  kind: "shell" | "command" | "tmux";
  exited?: boolean;
  exitCode?: number | null;
}

export function branchName(worktree: Worktree): string {
  return worktree.branch || `detached ${worktree.head.slice(0, 7)}`;
}

export function fuzzyScore(query: string, value: string): number {
  const needle = query.trim().toLowerCase();
  const haystack = value.toLowerCase();
  if (!needle) return 0;
  const direct = haystack.indexOf(needle);
  if (direct >= 0)
    return 1000 - direct - (haystack.length - needle.length) / 100;
  let cursor = -1;
  let score = 0;
  for (const char of needle) {
    if (char === " ") continue;
    const next = haystack.indexOf(char, cursor + 1);
    if (next < 0) return -1;
    score += next === cursor + 1 ? 12 : 1;
    if (next === 0 || "/-_. ".includes(haystack[next - 1])) score += 8;
    score -= (next - cursor - 1) / 10;
    cursor = next;
  }
  return score;
}

export function filterProjects(
  projects: Project[],
  query: string,
  projectId: string | null,
): Project[] {
  return projects
    .filter((project) => !projectId || project.id === projectId)
    .flatMap((project) => {
      if (fuzzyScore(query, `${project.name} ${project.id}`) >= 0)
        return [project];
      const worktrees = project.worktrees.filter(
        (worktree) =>
          fuzzyScore(query, `${branchName(worktree)} ${worktree.path}`) >= 0,
      );
      return worktrees.length ? [{ ...project, worktrees }] : [];
    });
}

export interface GraphGroup {
  project: Project;
  x: number;
  y: number;
  worktrees: { worktree: Worktree; x: number; y: number }[];
}

export function layoutGraph(projects: Project[]): {
  width: number;
  height: number;
  groups: GraphGroup[];
} {
  const columns = Math.max(1, Math.ceil(Math.sqrt(projects.length)));
  const groups: GraphGroup[] = [];
  let rowY = 30;
  for (let row = 0; row < Math.ceil(projects.length / columns); row++) {
    const rowProjects = projects.slice(row * columns, (row + 1) * columns);
    let rowHeight = 0;
    rowProjects.forEach((project, column) => {
      const x = 100 + column * 620;
      const height = Math.max(210, project.worktrees.length * 86);
      const y = rowY + height / 2;
      groups.push({
        project,
        x,
        y,
        worktrees: project.worktrees.map((worktree, index) => ({
          worktree,
          x: x + 160,
          y: rowY + 42 + index * 86,
        })),
      });
      rowHeight = Math.max(rowHeight, height);
    });
    rowY += rowHeight + 70;
  }
  return {
    width: Math.max(620, columns * 620),
    height: Math.max(360, rowY - 50),
    groups,
  };
}

// Arguments are passed directly to Bonsai. Quoting groups values; shell expansion is never performed.
export function parseArguments(input: string): string[] {
  const result: string[] = [];
  let current = "";
  let quote: string | null = null;
  let started = false;
  for (let index = 0; index < input.length; index++) {
    const char = input[index];
    if (char === "\\" && quote !== "'") {
      const next = input[index + 1];
      if (next === undefined)
        throw new Error("The final backslash needs a character after it.");
      if (quote === '"' && !["$", "`", '"', "\\", "\n"].includes(next)) {
        current += "\\";
        started = true;
        continue;
      }
      if (next !== "\n") {
        current += next;
        started = true;
      }
      index++;
      continue;
    }
    if (quote) {
      if (char === quote) quote = null;
      else current += char;
      continue;
    }
    if (char === '"' || char === "'") {
      quote = char;
      started = true;
      continue;
    }
    if (/\s/.test(char)) {
      if (started) {
        result.push(current);
        current = "";
        started = false;
      }
    } else {
      current += char;
      started = true;
    }
  }
  if (quote)
    throw new Error("Close the quoted argument before running the command.");
  if (started) result.push(current);
  return result;
}

export function escapeHtml(value: unknown): string {
  return String(value ?? "").replace(
    /[&<>"']/g,
    (char) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[
        char
      ]!,
  );
}

export function terminalInputChunks(input: Uint8Array): Uint8Array[] {
  const frameSize = 16 * 1024;
  return Array.from(
    { length: Math.ceil(input.length / frameSize) },
    (_, index) => input.subarray(index * frameSize, (index + 1) * frameSize),
  );
}


export const priorities: Priority[] = ["needs-you", "working", "recent", "older"];
export const priorityLabels: Record<Priority, string> = {
  "needs-you": "Needs you", working: "Working", recent: "Recent", older: "Older",
};

export function worktreeAgents(state: WorkspaceState, tree: Worktree): Agent[] {
  return (state.agents || []).filter((agent) => agent.worktreePath === tree.path);
}

export function worktreeSummary(state: WorkspaceState, tree: Worktree): Agent | null {
  const agents = worktreeAgents(state, tree);
  const attentionIds = new Set((state.attention || []).map((item) => item.agentId));
  return [...agents].sort((a, b) => {
    const rank = (agent: Agent) => attentionIds.has(agent.id) || (agent.state === "waiting" && !agent.stale) ? 0 : agent.live ? 1 : 2;
    return rank(a) - rank(b) || (b.updatedAt || 0) - (a.updatedAt || 0) || a.id.localeCompare(b.id);
  })[0] || null;
}

export function worktreePriority(state: WorkspaceState, tree: Worktree): Priority {
  if (tree.priority) return tree.priority;
  const agents = worktreeAgents(state, tree);
  if (agents.some((agent) => agent.state === "waiting" && !agent.stale)
    || (state.attention || []).some((item) => agents.some((agent) => agent.id === item.agentId))) return "needs-you";
  if (agents.some((agent) => agent.live) || runtimeActivity(tree).panes.length || runtimeActivity(tree).terminals.length) return "working";
  return tree.lastActivity ? "recent" : "older";
}

export function headquartersRows(state: WorkspaceState, query: string, projectId: string | null) {
  return state.projects.filter((project) => !projectId || project.id === projectId)
    .flatMap((project) => project.worktrees.map((worktree) => ({ project, worktree, priority: worktreePriority(state, worktree) })))
    .filter(({ project, worktree }) => {
      const needle = query.trim();
      const panes = worktree.activity?.tmux || [];
      const agents = worktreeAgents(state, worktree);
      if (/^%\d+$/.test(needle)) return panes.some((pane) => pane.pane === needle) || agents.some((agent) => agent.target?.tmuxPane === needle);
      if (/^@\d+$/.test(needle)) return panes.some((pane) => pane.window === needle);
      const fields = [project.name, project.id, worktree.path, branchName(worktree),
        ...agents.flatMap((agent) => [agent.provider, agent.title, agent.model, agent.state, agent.waitingReason, agent.target?.tmuxSession, agent.target?.tmuxPane]),
        ...panes.flatMap((pane) => [pane.session, pane.windowName, pane.window, pane.pane, pane.command]),
      ].filter((field): field is string => Boolean(field));
      return needle.split(/\s+/).every((word) => fields.some((field) => fuzzyScore(word, field) >= 0));
    })
    .sort((a, b) => priorities.indexOf(a.priority) - priorities.indexOf(b.priority)
      || (b.worktree.lastActivity || 0) - (a.worktree.lastActivity || 0)
      || a.worktree.path.localeCompare(b.worktree.path));
}

export function agentHierarchy(agents: Agent[]): { agent: Agent; depth: number }[] {
  const result: { agent: Agent; depth: number }[] = [];
  const seen = new Set<string>();
  const ordered = [...agents].sort((a, b) => a.id.localeCompare(b.id));
  function visit(agent: Agent, depth: number) {
    if (seen.has(agent.id)) return;
    seen.add(agent.id);
    result.push({ agent, depth });
    ordered.filter((child) => child.parentId === agent.id).forEach((child) => visit(child, depth + 1));
  }
  ordered.filter((agent) => !agent.parentId || !agents.some((parent) => parent.id === agent.parentId)).forEach((agent) => visit(agent, 0));
  ordered.forEach((agent) => visit(agent, 0));
  return result;
}

export function relativeTime(timestamp: number | null | undefined, now = Date.now() / 1000): string {
  if (!timestamp) return "—";
  const seconds = Math.max(0, now - timestamp);
  if (seconds < 60) return "now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h`;
  return `${Math.floor(seconds / 86400)}d`;
}
