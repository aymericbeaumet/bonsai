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
