import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import "./style.css";
import { activityBadges, inspectorActivity } from "./activity";
import {
  branchName,
  escapeHtml as h,
  filterProjects,
  fuzzyScore,
  layoutGraph,
  parseArguments,
  runtimeActivity,
  terminalInputChunks,
  type Project,
  type Session,
  type WorkspaceState,
  type Worktree,
} from "./model";

const $ = <T extends Element = HTMLElement>(selector: string): T =>
  document.querySelector<T>(selector)!;
const icons: Record<string, string> = {
  tree: '<path d="M12 21V8m0 7L5 8m7 2 7-6"/><circle cx="5" cy="6" r="2"/><circle cx="12" cy="5" r="2"/><circle cx="20" cy="3" r="2"/>',
  search: '<circle cx="10.5" cy="10.5" r="6.5"/><path d="m16 16 4 4"/>',
  grid: '<rect x="3" y="3" width="6" height="6" rx="1"/><rect x="15" y="3" width="6" height="6" rx="1"/><rect x="3" y="15" width="6" height="6" rx="1"/><rect x="15" y="15" width="6" height="6" rx="1"/>',
  graph:
    '<circle cx="5" cy="12" r="3"/><circle cx="19" cy="5" r="3"/><circle cx="19" cy="19" r="3"/><path d="m8 10 8-4M8 14l8 4"/>',
  branch:
    '<path d="M6 6v12m12-12v3a6 6 0 0 1-6 6H6"/><circle cx="6" cy="4" r="2"/><circle cx="6" cy="20" r="2"/><circle cx="18" cy="4" r="2"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  terminal: '<path d="m5 7 5 5-5 5m8 0h6"/>',
  arrow: '<path d="M5 12h14m-5-5 5 5-5 5"/>',
  chevron: '<path d="m9 5 7 7-7 7"/>',
  refresh: '<path d="M20 7V3l-3 3a8 8 0 1 0 3 9M20 3h-5"/>',
  close: '<path d="m6 6 12 12M6 18 18 6"/>',
  expand: '<path d="M8 3H3v5m13-5h5v5M3 16v5h5m13-5v5h-5"/>',
  minus: '<path d="M5 12h14"/>',
  command:
    '<path d="M8 8h8v8H8zm0 0H5a3 3 0 1 1 3-3v3m8 0V5a3 3 0 1 1 3 3h-3m0 8h3a3 3 0 1 1-3 3v-3m-8 0v3a3 3 0 1 1-3-3h3"/>',
  folder: '<path d="M3 6a2 2 0 0 1 2-2h5l2 3h7a2 2 0 0 1 2 2v10H3Z"/>',
  layers: '<path d="m12 3 10 5-10 5L2 8Zm-10 9 10 5 10-5M2 16l10 5 10-5"/>',
  trash: '<path d="M3 6h18M9 6V3h6v3M6 6l1 15h10l1-15M10 10v7m4-7v7"/>',
  check: '<path d="m5 12 4 4L19 6"/>',
  clock: '<circle cx="12" cy="12" r="9"/><path d="M12 6v6l4 2"/>',
  copy: '<rect x="8" y="8" width="12" height="13" rx="2"/><path d="M16 8V3H3v13h5"/>',
  globe:
    '<circle cx="12" cy="12" r="9"/><ellipse cx="12" cy="12" rx="4" ry="9"/><path d="M3 12h18"/>',
};
const icon = (name: string, extra = "") =>
  `<svg class="icon ${extra}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.55" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${icons[name] || icons.command}</svg>`;
const short = (value: string, max = 25) =>
  value.length > max ? `${value.slice(0, max - 1)}…` : value;
const plural = (count: number, word: string) =>
  `${count} ${word}${count === 1 ? "" : "s"}`;

const fragment = new URLSearchParams(location.hash.slice(1));
let token =
  fragment.get("token") || sessionStorage.getItem("bonsai.token") || "";
if (fragment.has("token")) {
  sessionStorage.setItem("bonsai.token", token);
  history.replaceState(null, "", location.pathname + location.search);
}

let state: WorkspaceState | null = null;
let selectedPath = localStorage.getItem("bonsai.selected") || "";
let projectFilter: string | null = null;
let inspectorMobileOpen = false;
let query = "";
let view: "graph" | "list" =
  localStorage.getItem("bonsai.view") === "list" ? "list" : "graph";
let loading = false;
let connected = false;
let refreshTimer: ReturnType<typeof setTimeout>;
let toastTimer: ReturnType<typeof setTimeout>;
let graphScale = 1;
let graphFitted = true;
let graphX = 0;
let graphY = 0;
let graphKey = "";
let selectedTerminal: string | null = null;
let dockOpen = false;
let dockExpanded = false;
let lastState = "";
const sessions = new Map<string, Session>();
interface LiveTerminal {
  terminal: Terminal;
  fit: FitAddon;
  socket: WebSocket | null;
  element: HTMLDivElement;
  status: "connecting" | "live" | "offline" | "exited";
}
const terminals = new Map<string, LiveTerminal>();

$("#app").innerHTML = `
  <aside class="sidebar" aria-label="Projects">
    <a href="/" class="brand" aria-label="Bonsai home"><span class="brand-mark">${icon("tree")}</span><span>bonsai<span class="brand-dot">.</span></span><span class="brand-tag">WORKSPACE</span></a>
    <button class="search-launch" data-action="palette">${icon("search")}<span>Find anything</span><kbd>⌘ K</kbd></button>
    <div class="nav-section-label">EXPLORER <span id="project-count">—</span></div>
    <button class="nav-item all-projects active" data-action="all">${icon("globe")}<span>All projects</span><span id="total-trees" class="nav-count">—</span></button>
    <div id="project-nav" class="project-nav"><div class="nav-skeleton"></div><div class="nav-skeleton"></div><div class="nav-skeleton"></div></div>
    <div class="sidebar-divider"></div>
    <div class="nav-section-label">TOOLS</div>
    <button class="nav-item" data-action="command">${icon("command")}<span>Command center</span><span class="subtle">↗</span></button>
    <button class="nav-item" data-action="sessions">${icon("terminal")}<span>tmux sessions</span><span id="tmux-count" class="nav-count">—</span></button>
    <button class="nav-item" data-action="clean">${icon("layers")}<span>Clean up</span></button>
    <div class="sidebar-bottom"><span class="connection-dot"></span><div><strong id="connection-text">Connecting</strong><span id="workspace-root" title="Workspace root">Local workspace</span></div><button class="icon-button" data-action="refresh" title="Refresh workspace" aria-label="Refresh workspace">${icon("refresh")}</button></div>
  </aside>
  <main class="main">
    <header class="topbar"><div class="breadcrumbs"><button class="icon-button mobile-menu" data-action="sidebar" aria-label="Toggle projects">${icon("layers")}</button><span>Workspace</span>${icon("chevron")}<strong id="breadcrumb">All projects</strong></div><div class="topbar-right"><span class="local-badge"><span></span> LOCAL</span><button class="button primary small" data-action="add">${icon("plus")} New worktree</button></div></header>
    <section class="workspace">
      <div class="workspace-heading"><div><div class="eyebrow">A LITTLE SPACE FOR BIG IDEAS</div><h1>Your constellation<span>.</span></h1><p id="workspace-subtitle">Every project. Every branch. Room to grow.</p></div><div class="view-toggle" aria-label="Workspace view"><button data-view="graph" title="Graph view">${icon("graph")}<span>Graph</span></button><button data-view="list" title="List view">${icon("layers")}<span>List</span></button></div></div>
      <div class="workspace-toolbar"><div class="workspace-stats" id="workspace-stats"><span>Discovering your workspaces…</span></div><label class="inline-search">${icon("search")}<input id="filter" type="search" placeholder="Filter branches, projects…" aria-label="Filter branches and projects"><kbd>/</kbd></label></div>
      <div id="warning-banner" class="warning-banner" hidden></div>
      <div class="canvas-layout"><div id="canvas" class="canvas"><div class="empty-state"><span class="loading-orbit"></span><h2>Finding your constellation</h2><p>Discovering projects and their worktrees.</p></div></div><aside id="inspector" class="inspector" aria-label="Worktree details"></aside></div>
    </section>
    <section id="terminal-dock" class="terminal-dock" aria-label="Browser terminals"><div class="terminal-header"><button class="terminal-label" data-action="toggle-dock">${icon("terminal")}<span>Terminal</span><span id="session-count" class="terminal-count">0</span></button><div id="terminal-tabs" class="terminal-tabs" role="tablist" aria-label="Terminal sessions"></div><div class="terminal-actions"><button class="icon-button" data-action="shell" title="New shell" aria-label="New shell">${icon("plus")}</button><button class="icon-button" data-action="expand-dock" title="Expand terminal" aria-label="Expand terminal">${icon("expand")}</button><button class="icon-button" data-action="toggle-dock" title="Toggle terminal panel" aria-label="Toggle terminal panel">${icon("minus")}</button></div></div><div id="terminal-context" class="terminal-context"></div><div id="terminal-body" class="terminal-body"><div class="terminal-empty"><span>${icon("terminal")}</span><h3>Make yourself at home.</h3><p>Your shell, editors, and coding agents. Right here.</p><button class="button primary" data-action="shell">${icon("terminal")} Open a terminal</button></div></div></section>
    <footer class="statusbar"><span>${icon("tree")}<span id="status-summary">Connecting to Bonsai</span></span><span><span class="status-hint">Drag to explore · Scroll to zoom</span><button data-action="palette"><kbd>⌘ K</kbd> Jump anywhere</button></span></footer>
  </main>`;

function toast(message: string, error = false): void {
  const element = $("#toast");
  element.textContent = message;
  element.className = `toast visible${error ? " error" : ""}`;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(
    () => element.classList.remove("visible"),
    error ? 6500 : 3500,
  );
}

async function api<T>(path: string, options: RequestInit = {}): Promise<T> {
  const response = await fetch(path, {
    ...options,
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
      ...options.headers,
    },
  });
  if (!response.ok) {
    const body = await response
      .json()
      .catch(() => ({ error: response.statusText }));
    throw new Error(body.error || `Request failed (${response.status})`);
  }
  if (response.status === 204) return undefined as T;
  return response.json() as Promise<T>;
}

function selected(): { project: Project; worktree: Worktree } | null {
  for (const project of state?.projects || []) {
    const worktree = project.worktrees.find(
      (tree) => tree.path === selectedPath,
    );
    if (worktree) return { project, worktree };
  }
  return null;
}

function contextPath(): string {
  const current = selected();
  return (
    (current?.worktree.prunable
      ? current.project.worktrees.find((tree) => !tree.prunable)?.path
      : current?.worktree.path) ||
    state?.projects.find((project) => project.id === projectFilter)?.path ||
    state?.projects[0]?.path ||
    state?.root ||
    ""
  );
}

async function refresh(): Promise<void> {
  if (loading) return;
  loading = true;
  clearTimeout(refreshTimer);
  try {
    const next = await api<WorkspaceState>("/api/state");
    state = next;
    connected = true;
    if (!selected()) selectedPath = next.projects[0]?.worktrees[0]?.path || "";
    if (
      projectFilter &&
      !next.projects.some((project) => project.id === projectFilter)
    )
      projectFilter = null;
    const signature = JSON.stringify(next);
    if (signature !== lastState) {
      lastState = signature;
      renderWorkspace();
    }
    $("#connection-text").textContent = "Connected";
    $("#workspace-root").textContent = next.root;
    $("#workspace-root").setAttribute("title", next.root);
    document.body.classList.add("connected");
    await restoreSessions();
  } catch (error) {
    connected = false;
    document.body.classList.remove("connected");
    $("#connection-text").textContent = "Disconnected";
    $("#status-summary").textContent = "Connection lost · retrying";
    if (!state) {
      $("#canvas").innerHTML =
        `<div class="empty-state"><span class="empty-icon">${icon("globe")}</span><h2>${token ? "Let’s reconnect" : "Connect to your workspace"}</h2><p>${h(token ? (error instanceof Error ? error.message : String(error)) : "Open the secure browser link printed by bonsai hq, or paste its access token below.")}</p><form id="connect-form"><input name="token" type="password" placeholder="Access token" aria-label="Access token" autocomplete="off" required><button class="button primary">Connect</button></form><button class="button ghost" data-action="refresh">${icon("refresh")} Try again</button></div>`;
      $("#connect-form")?.addEventListener("submit", (event) => {
        event.preventDefault();
        token = String(
          new FormData(event.currentTarget as HTMLFormElement).get("token"),
        ).trim();
        sessionStorage.setItem("bonsai.token", token);
        void refresh();
      });
    }
  } finally {
    loading = false;
    refreshTimer = setTimeout(() => void refresh(), 5000);
  }
}

function renderWorkspace(): void {
  if (!state) return;
  const total = state.projects.reduce(
    (sum, project) => sum + project.worktrees.length,
    0,
  );
  const dirty = state.projects
    .flatMap((project) => project.worktrees)
    .filter((tree) => tree.dirty).length;
  $("#project-count").textContent = String(state.projects.length).padStart(
    2,
    "0",
  );
  $("#total-trees").textContent = String(total);
  $("#tmux-count").textContent = String(state.tmux.sessions.length);
  $("#project-nav").innerHTML =
    state.projects
      .map(
        (project, index) =>
          `<button class="nav-item project-item ${projectFilter === project.id ? "active" : ""}" data-project="${h(project.id)}" title="${h(project.id)}"><span class="project-glyph color-${index % 5}">${h(project.name.slice(0, 1).toUpperCase())}</span><span>${h(project.name)}</span><span class="nav-count">${project.worktrees.length}</span></button>`,
      )
      .join("") || '<p class="nav-empty">Your projects will appear here.</p>';
  $(".all-projects").classList.toggle("active", !projectFilter);
  $("#breadcrumb").textContent =
    state.projects.find((project) => project.id === projectFilter)?.name ||
    "All projects";
  $("#workspace-subtitle").textContent = projectFilter
    ? state.projects.find((project) => project.id === projectFilter)?.id || ""
    : "Every project. Every branch. Room to grow.";
  $("#workspace-stats").innerHTML =
    `<span>${icon("folder")}<strong>${state.projects.length}</strong> projects</span><span>${icon("branch")}<strong>${total}</strong> worktrees</span><span><i class="dot amber"></i><strong>${dirty}</strong> with changes</span>`;
  $("#status-summary").textContent =
    `${plural(state.projects.length, "project")} · ${plural(total, "worktree")}`;
  const warning = $("#warning-banner");
  warning.hidden = !state.warnings.length;
  warning.innerHTML = state.warnings.length
    ? `<details><summary>${plural(state.warnings.length, "workspace notice")}</summary>${state.warnings.map((message) => `<p>${h(message)}</p>`).join("")}</details>`
    : "";
  document
    .querySelectorAll<HTMLButtonElement>("[data-view]")
    .forEach((button) => {
      button.classList.toggle("active", button.dataset.view === view);
      button.setAttribute("aria-pressed", String(button.dataset.view === view));
    });
  renderCanvas();
  renderInspector();
}

function renderCanvas(): void {
  if (!state) return;
  const projects = filterProjects(state.projects, query, projectFilter);
  const canvas = $("#canvas");
  if (!projects.length) {
    canvas.innerHTML = `<div class="empty-state"><span class="empty-icon">${icon(query ? "search" : "tree")}</span><div class="eyebrow">${query ? "KEEP EXPLORING" : "GREAT THINGS START SMALL"}</div><h2>${query ? "No branches in sight" : "A place for your next idea"}</h2><p>${query ? "Try a project name, branch, or part of a path." : "Create a worktree from a local Git project and watch your workspace take shape."}</p><button class="button primary" data-action="${query ? "clear-filter" : "add"}">${icon(query ? "refresh" : "plus")}${query ? "Clear filters" : "Create your first worktree"}</button>${!query ? '<button class="button ghost" data-action="shell">Or open a terminal to get started</button>' : ""}</div>`;
    return;
  }
  if (view === "list") {
    canvas.innerHTML = `<div class="worktree-list"><div class="list-heading"><span>PROJECT / BRANCH</span><span>STATUS</span><span>SYNC</span><span></span></div>${projects.map((project) => `<div class="list-project-label">${icon("folder")} ${h(project.name)}<span>${h(project.id)}</span></div>${project.worktrees.map((tree) => `<button class="worktree-row ${selectedPath === tree.path ? "selected" : ""}" data-tree="${h(tree.path)}"><span class="list-branch">${icon("branch")}<span><strong>${h(branchName(tree))}</strong><small>${h(tree.path)}</small>${activityBadges(tree)}</span>${tree.main ? '<span class="mini-tag">main checkout</span>' : ""}</span><span class="list-status ${tree.dirty ? "changed" : ""}"><i class="dot ${tree.prunable || tree.dirty === null ? "muted-dot" : tree.dirty ? "amber" : ""}"></i>${tree.prunable ? "Missing" : tree.dirty === null ? "Unavailable" : tree.dirty ? "Changes" : "Clean"}</span><span class="list-sync">${tree.ahead ? `↑ ${tree.ahead}` : ""} ${tree.behind ? `↓ ${tree.behind}` : ""}${!tree.ahead && !tree.behind ? "—" : ""}</span><span class="row-open" data-open-shell="${h(tree.path)}" title="Open terminal">${icon("terminal")}</span></button>`).join("")}`).join("")}</div>`;
    return;
  }
  const layout = layoutGraph(projects);
  const key = JSON.stringify(
    projects.map((project) => [
      project.id,
      project.worktrees.map((tree) => tree.path),
    ]),
  );
  const shouldFit = key !== graphKey;
  graphKey = key;
  canvas.innerHTML = `<svg id="constellation" class="constellation" role="group" aria-label="Project and worktree constellation"><g id="graph-transform">${layout.groups
    .map((group, index) => {
      const color = ["#bfda91", "#a3c5c1", "#c1b1d7", "#d9b58b", "#9ab7d9"][
        index % 5
      ];
      return `<g class="project-cluster" style="--cluster:${color}">${group.worktrees.map((node) => `<path class="graph-edge ${selectedPath === node.worktree.path ? "selected" : ""}" d="M ${group.x + 34} ${group.y} C ${group.x + 130} ${group.y}, ${node.x - 110} ${node.y}, ${node.x} ${node.y}"/>`).join("")}<g class="project-node" transform="translate(${group.x},${group.y})" data-project-node="${h(group.project.id)}" tabindex="0" role="button" aria-label="Focus project ${h(group.project.name)}"><circle class="project-halo" r="49"/><circle class="project-orbit" r="40"/><circle class="project-core" r="30"/><path d="M-8-10v20m0-8h8a8 8 0 0 0 8-8v-4" stroke="${color}" stroke-width="2" fill="none"/><circle cx="-8" cy="-12" r="3" fill="${color}"/><circle cx="-8" cy="12" r="3" fill="${color}"/><circle cx="8" cy="-12" r="3" fill="${color}"/><text class="project-node-title" y="73" text-anchor="middle">${h(short(group.project.name, 25))}</text><text class="project-node-meta" y="94" text-anchor="middle">${plural(group.project.worktrees.length, "worktree")}</text></g>${group.worktrees
        .map((node) => {
          const tree = node.worktree;
          const active = tree.path === selectedPath;
          const activity = runtimeActivity(tree);
          return `<g class="tree-node ${active ? "selected" : ""}" transform="translate(${node.x},${node.y})" data-tree="${h(tree.path)}" tabindex="0" role="button" aria-label="${h(group.project.name)} ${h(branchName(tree))}${tree.dirty ? ", uncommitted changes" : ""}" aria-pressed="${active}"><title>${h(branchName(tree))}\n${h(tree.path)}${activity.summary ? `\n${h(activity.summary)}` : ""}</title><rect class="tree-node-halo" x="-4" y="-35" width="268" height="${activity.summary ? 86 : 70}" rx="13"/><rect class="tree-node-card" y="-31" width="260" height="${activity.summary ? 78 : 62}" rx="9"/><circle class="node-port" r="3"/><path class="node-branch-icon" d="M18-8v16m0-6h5a5 5 0 0 0 5-5v-5"/><circle class="node-status ${tree.dirty ? "dirty" : ""} ${tree.prunable || tree.dirty === null ? "unknown" : ""}" cx="240" cy="0" r="3.5"/><text class="node-title" data-label="${h(branchName(tree))}" x="41" y="-4">${h(short(branchName(tree), 23))}</text><text class="node-meta" x="41" y="17">${h(tree.prunable ? "MISSING DIRECTORY" : tree.dirty === null ? "STATUS UNAVAILABLE" : tree.main ? "MAIN CHECKOUT" : tree.external ? "EXTERNAL WORKTREE" : tree.locked ? "LOCKED" : tree.head.slice(0, 7))}${tree.ahead ? ` · ↑${tree.ahead}` : ""}${tree.behind ? ` · ↓${tree.behind}` : ""}</text>${activity.summary ? `<text class="node-activity" data-label="${h(activity.summary)}" x="41" y="37" data-terminal-count="${activity.terminals.length}" data-tmux-count="${activity.panes.length}">${h(activity.summary)}</text>` : ""}</g>`;
        })
        .join("")}</g>`;
    })
    .join(
      "",
    )}</g></svg><div class="canvas-caption"><span class="tiny-star">✳</span> A BIRD’S-EYE VIEW OF YOUR WORK</div><div class="graph-controls"><button class="icon-button" data-action="zoom-in" title="Zoom in" aria-label="Zoom in">${icon("plus")}</button><span id="zoom-label">100%</span><button class="icon-button" data-action="zoom-out" title="Zoom out" aria-label="Zoom out">${icon("minus")}</button><span class="control-divider"></span><button class="icon-button" data-action="fit" title="Fit all projects" aria-label="Fit all projects">${icon("expand")}</button></div><div class="graph-legend"><span><i class="dot"></i> Clean</span><span><i class="dot amber"></i> Changes</span></div>`;
  if (shouldFit) fitGraph();
  else applyGraphTransform();
  setupGraphInteractions();
}

function applyGraphTransform(): void {
  $("#graph-transform")?.setAttribute(
    "transform",
    `translate(${graphX} ${graphY}) scale(${graphScale})`,
  );
  if ($("#zoom-label"))
    $("#zoom-label").textContent = `${Math.round(graphScale * 100)}%`;
  document.querySelectorAll<SVGTextElement>(".node-title").forEach((label) => {
    const fontSize = Math.max(14, 11 / Math.max(graphScale, 0.25));
    label.style.fontSize = `${fontSize}px`;
    label.textContent = short(
      label.dataset.label || "",
      Math.max(2, Math.floor(186 / (fontSize * 0.61))),
    );
  });
  document
    .querySelectorAll<SVGTextElement>(".node-meta, .node-activity")
    .forEach((label) => {
      const fontSize = Math.max(10, 8 / Math.max(graphScale, 0.25));
      label.style.fontSize = `${fontSize}px`;
      if (label.classList.contains("node-activity")) {
        const text = label.dataset.label || "";
        label.textContent =
          text.length * fontSize * 0.6 <= 186
            ? text
            : [
                ...(Number(label.dataset.terminalCount)
                  ? [`T${label.dataset.terminalCount}`]
                  : []),
                ...(Number(label.dataset.tmuxCount)
                  ? [`tmux${label.dataset.tmuxCount}`]
                  : []),
              ].join(" · ");
      }
    });
  document
    .querySelectorAll<SVGTextElement>(".project-node-title")
    .forEach((label) => {
      label.style.fontSize = `${Math.max(17, 12 / Math.max(graphScale, 0.25))}px`;
    });
  document
    .querySelectorAll<SVGTextElement>(".project-node-meta")
    .forEach((label) => {
      label.style.fontSize = `${Math.max(11, 9 / Math.max(graphScale, 0.25))}px`;
    });
}

function fitGraph(): void {
  graphFitted = true;
  if (!state || view !== "graph") return;
  const layout = layoutGraph(
    filterProjects(state.projects, query, projectFilter),
  );
  const rect = $("#canvas").getBoundingClientRect();
  graphScale = Math.max(
    0.08,
    Math.min(
      1.3,
      (rect.width - 60) / layout.width,
      (rect.height - 70) / layout.height,
    ),
  );
  graphX = (rect.width - layout.width * graphScale) / 2;
  graphY = (rect.height - layout.height * graphScale) / 2;
  applyGraphTransform();
}

function zoom(factor: number, x?: number, y?: number): void {
  graphFitted = false;
  const rect = $("#canvas").getBoundingClientRect();
  const pivotX = x ?? rect.width / 2;
  const pivotY = y ?? rect.height / 2;
  const next = Math.max(0.05, Math.min(3, graphScale * factor));
  graphX = pivotX - ((pivotX - graphX) * next) / graphScale;
  graphY = pivotY - ((pivotY - graphY) * next) / graphScale;
  graphScale = next;
  applyGraphTransform();
}

function setupGraphInteractions(): void {
  const svg = $<SVGSVGElement>("#constellation");
  let drag: { x: number; y: number; originX: number; originY: number } | null =
    null;
  svg.addEventListener(
    "wheel",
    (event) => {
      event.preventDefault();
      const rect = svg.getBoundingClientRect();
      zoom(
        Math.exp(-event.deltaY * 0.0015),
        event.clientX - rect.left,
        event.clientY - rect.top,
      );
    },
    { passive: false },
  );
  svg.addEventListener("pointerdown", (event) => {
    if ((event.target as Element).closest("[data-tree], [data-project-node]"))
      return;
    drag = {
      x: event.clientX,
      y: event.clientY,
      originX: graphX,
      originY: graphY,
    };
    svg.setPointerCapture(event.pointerId);
    svg.classList.add("dragging");
  });
  svg.addEventListener("pointermove", (event) => {
    if (!drag) return;
    graphFitted = false;
    graphX = drag.originX + event.clientX - drag.x;
    graphY = drag.originY + event.clientY - drag.y;
    applyGraphTransform();
  });
  const end = () => {
    drag = null;
    svg.classList.remove("dragging");
  };
  svg.addEventListener("pointerup", end);
  svg.addEventListener("pointercancel", end);
  svg.addEventListener("dblclick", (event) => {
    const tree = (event.target as Element).closest<SVGElement>("[data-tree]");
    if (tree) void createSession({ path: tree.dataset.tree! });
    else fitGraph();
  });
}

function selectTree(path: string): void {
  inspectorMobileOpen = true;
  selectedPath = path;
  localStorage.setItem("bonsai.selected", path);
  renderCanvas();
  renderInspector();
  $("#inspector").scrollTop = 0;
}

function renderInspector(): void {
  const current = selected();
  const inspector = $("#inspector");
  inspector.classList.toggle(
    "has-selection",
    Boolean(current) &&
      (inspectorMobileOpen || matchMedia("(min-width: 581px)").matches),
  );
  if (!current) {
    inspector.innerHTML =
      '<div class="inspector-placeholder">Select a worktree<br>to explore its possibilities.</div>';
    return;
  }
  const { project, worktree: tree } = current;
  const changed = tree.added + tree.modified + tree.deleted + tree.untracked;
  inspector.innerHTML = `<div class="inspector-topline"><span>WORKTREE DETAILS</span><button class="icon-button inspector-close" data-action="close-inspector" aria-label="Close details">${icon("close")}</button></div><div class="inspector-branch-icon">${icon("branch")}</div><div class="inspector-project">${h(project.name)}</div><h2 class="inspector-title">${h(branchName(tree))}</h2><div class="badge-row"><span class="badge ${tree.prunable || tree.dirty === null ? "neutral" : tree.dirty ? "changed" : "clean"}"><i class="dot ${tree.prunable || tree.dirty === null ? "muted-dot" : tree.dirty ? "amber" : ""}"></i>${tree.prunable ? "Missing directory" : tree.dirty === null ? "Status unavailable" : tree.dirty ? "Uncommitted changes" : "Working tree clean"}</span>${tree.main ? '<span class="badge neutral">Main checkout</span>' : ""}${tree.external ? '<span class="badge neutral">External</span>' : ""}${tree.locked ? '<span class="badge neutral">Locked</span>' : ""}</div><button class="button primary full" data-action="shell" ${tree.prunable ? "disabled" : ""}>${icon("terminal")} Open terminal <span class="button-end">↗</span></button><button class="button secondary full" data-action="new-tmux" ${!state?.tmux.available || tree.prunable ? "disabled" : ""} title="${state?.tmux.available ? "Create or attach a persistent tmux session" : "tmux is not installed"}">${icon("layers")} Persistent tmux session</button>${inspectorActivity(tree)}<div class="inspector-section"><div class="section-caption">CHECKOUT</div><div class="detail-line"><span>Commit</span><code>${h(tree.head.slice(0, 8)) || "—"}</code></div><div class="detail-line"><span>Upstream</span><span class="sync-detail">${tree.ahead || tree.behind ? `↑ ${tree.ahead} ahead <span>·</span> ↓ ${tree.behind} behind` : "No pending commits"}</span></div><button class="path-copy" data-action="copy-path" title="Copy worktree path"><span>${h(tree.path)}</span>${icon("copy")}</button></div><div class="inspector-section"><div class="section-caption">WORKING CHANGES <span>${changed}</span></div><div class="changes-grid"><div><span class="added">+${tree.added}</span><small>Added</small></div><div><span class="modified">${tree.modified}</span><small>Modified</small></div><div><span class="deleted">−${tree.deleted}</span><small>Deleted</small></div><div><span>${tree.untracked}</span><small>Untracked</small></div></div></div><div class="inspector-section inspector-actions"><div class="section-caption">MAKE YOUR NEXT MOVE</div><button data-action="add-from-here">${icon("plus")} Branch from here ${icon("chevron")}</button><button data-action="start">${icon("terminal")} New coding session ${icon("chevron")}</button><button data-action="resume">${icon("clock")} Resume a coding session ${icon("chevron")}</button><button data-action="command">${icon("command")} Run a Bonsai command ${icon("chevron")}</button><button data-action="workspace">${icon("folder")} Generate editor workspace ${icon("chevron")}</button>${!tree.main && !tree.external ? `<button class="danger-text" data-action="remove" ${tree.locked ? "disabled" : ""}>${icon("trash")} Remove worktree ${icon("chevron")}</button>` : ""}</div><div class="inspector-note">${icon("tree")} A branch is a fresh possibility.</div>`;
}

const commandDefinitions = [
  ["add", "Create or reuse a worktree", "ab/my-feature --base main"],
  [
    "list",
    "List project worktrees or use --all for every project",
    "--all --status",
  ],
  ["cd", "Find a worktree and print its path", ""],
  [
    "start",
    "Start a new Claude Code, Codex, or OpenCode session",
    'codex --prompt "Build something great"',
  ],
  ["resume", "Resume Claude Code, Codex, or OpenCode interactively", ""],
  ["workspace", "Generate your editor workspace", "--all"],
  [
    "clean",
    "Review and remove merged worktrees; dirty trees are skipped",
    "--dry-run",
  ],
  ["prune", "Remove stale registrations and orphaned directories", "--all"],
  [
    "remove",
    "Remove worktrees, preserving branches unless -d is given",
    "ab/my-feature",
  ],
  ["init", "Print the Bonsai shell integration", "zsh"],
  ["agents", "Print shared agent instructions", ""],
  ["skill", "Print or install the bundled skill", "install"],
  ["completions", "Print completions for your shell", "zsh"],
  ["--help", "Explore every Bonsai command and option", ""],
  ["--version", "Show the installed Bonsai version", ""],
] as const;

function modal(content: string, className = ""): HTMLDialogElement {
  const dialog = $<HTMLDialogElement>("#modal");
  dialog.className = className;
  dialog.innerHTML = content;
  if (!dialog.open) dialog.showModal();
  return dialog;
}
const modalHeader = (eyebrow: string, title: string) =>
  `<div class="modal-heading"><div><div class="eyebrow">${eyebrow}</div><h2 id="modal-title">${title}</h2></div><button class="icon-button" data-action="close-modal" aria-label="Close dialog">${icon("close")}</button></div>`;
const pathField = (value = contextPath()) =>
  `<label>Working directory<input name="cwd" value="${h(value)}" list="workspace-paths" placeholder="/absolute/path/to/project" required autocomplete="off"></label><datalist id="workspace-paths">${
    state?.projects
      .flatMap((project) => [
        project.path,
        ...project.worktrees.map((tree) => tree.path),
      ])
      .map((path) => `<option value="${h(path)}"></option>`)
      .join("") || ""
  }</datalist>`;

function submitForm(
  form: HTMLFormElement,
  operation: (data: FormData) => Promise<void>,
): void {
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const button = form.querySelector<HTMLButtonElement>('[type="submit"]')!;
    const error = form.querySelector<HTMLElement>(".form-error")!;
    button.disabled = true;
    error.textContent = "";
    try {
      await operation(new FormData(form));
      $<HTMLDialogElement>("#modal").close();
    } catch (failure) {
      error.textContent =
        failure instanceof Error ? failure.message : String(failure);
    } finally {
      button.disabled = false;
    }
  });
}

function openAdd(base = ""): void {
  const dialog = modal(
    `${modalHeader("ROOM TO GROW", "A fresh branch, a fresh space.")}<p class="modal-description">Create an isolated worktree and keep your current work right where it is.</p><form id="add-form">${pathField()}<label>Branch name<input name="branch" placeholder="ab/your-next-idea" required autocomplete="off" autofocus></label><div class="form-row"><label>Base reference <span class="optional">optional</span><input name="base" value="${h(base)}" placeholder="Default branch"></label><label>Custom path <span class="optional">optional</span><input name="path" placeholder="Inside project workspace"></label></div><label class="checkbox-row"><input type="checkbox" name="fetch"> Fetch even if disabled in project configuration</label><p class="form-error" role="alert"></p><div class="modal-footer"><span>Output opens in your terminal dock.</span><button class="button primary" type="submit">${icon("plus")} Create worktree</button></div></form>`,
  );
  submitForm(dialog.querySelector("form")!, async (data) => {
    const args = ["add", String(data.get("branch")).trim()];
    if (String(data.get("base")).trim())
      args.push("--base", String(data.get("base")).trim());
    if (String(data.get("path")).trim())
      args.push("--path", String(data.get("path")).trim());
    if (data.has("fetch")) args.push("--fetch");
    await createSession({ path: String(data.get("cwd")).trim(), args });
  });
}

function openRemove(): void {
  const current = selected();
  if (!current || current.worktree.main || current.worktree.external) return;
  const tree = current.worktree;
  const dialog = modal(
    `${modalHeader("MAKE SOME ROOM", "Remove this worktree")}<p class="modal-description">Remove <strong>${h(branchName(tree))}</strong> from ${h(current.project.name)}. Your branch is kept unless you choose to delete it.</p><div class="form-path">${h(tree.path)}</div><form id="remove-form"><label class="checkbox-row"><input type="checkbox" name="delete"> Also delete the Git branch</label><label class="checkbox-row danger-check"><input type="checkbox" name="force"> Discard uncommitted changes and allow deletion of an unmerged branch</label>${tree.dirty ? '<p class="inline-notice">This worktree has uncommitted changes. Removal will refuse them unless you explicitly choose to discard them above.</p>' : ""}<p class="form-error" role="alert"></p><div class="modal-footer"><button class="button secondary" type="button" data-action="close-modal">Keep worktree</button><button class="button danger" type="submit">${icon("trash")} Remove worktree</button></div></form>`,
  );
  submitForm(dialog.querySelector("form")!, async (data) => {
    const args = ["remove"];
    if (data.has("delete")) args.push("--delete-branch");
    if (data.has("force")) args.push("--force");
    args.push("--", tree.path);
    await createSession({ path: current.project.path, args });
  });
}

function openCommand(command = "list", initialArgs?: string): void {
  const definition =
    commandDefinitions.find((item) => item[0] === command) ||
    commandDefinitions[0];
  const dialog = modal(
    `${modalHeader("THE COMMAND CENTER", "All of Bonsai. At your fingertips.")}<p class="modal-description">Commands run in a full terminal, including interactive pickers and confirmation prompts.</p><form id="command-form">${pathField()}<label>Command<select name="command" id="command-select">${commandDefinitions.map((item) => `<option value="${item[0]}" ${item[0] === command ? "selected" : ""}>bonsai ${item[0]}</option>`).join("")}</select></label><p id="command-description" class="field-description">${h(definition[1])}</p><label>Arguments<input name="args" id="command-args" value="${h(initialArgs ?? (command === "clean" ? "--dry-run" : command === "init" || command === "completions" ? "zsh" : command === "list" ? "--all --status" : ""))}" placeholder="${h(definition[2])}" autocomplete="off"></label><p class="field-description">Use quotes around arguments containing spaces. Use --help for command options.</p><p class="form-error" role="alert"></p><div class="modal-footer"><button class="button secondary" type="button" id="command-help">Command help</button><button class="button primary" type="submit">${icon("terminal")} Run command</button></div></form>`,
  );
  const select = dialog.querySelector<HTMLSelectElement>("#command-select")!;
  select.addEventListener("change", () => {
    const next = commandDefinitions.find((item) => item[0] === select.value)!;
    $("#command-description").textContent = next[1];
    $<HTMLInputElement>("#command-args").value =
      select.value === "clean"
        ? "--dry-run"
        : select.value === "init" || select.value === "completions"
          ? "zsh"
          : "";
    $<HTMLInputElement>("#command-args").placeholder = next[2];
  });
  $("#command-help").addEventListener("click", () => {
    $<HTMLInputElement>("#command-args").value = "--help";
  });
  submitForm(dialog.querySelector("form")!, async (data) => {
    const args = [
      String(data.get("command")),
      ...parseArguments(String(data.get("args"))),
    ];
    await createSession({ path: String(data.get("cwd")).trim(), args });
  });
}

function openClean(): void {
  const dialog = modal(
    `${modalHeader("A LITTLE GARDENING", "Keep your workspace growing.")}<p class="modal-description">Bonsai finds merged branches, including squash merges. Uncommitted work is always preserved.</p><form id="clean-form">${pathField()}<div class="clean-options"><label><input type="radio" name="mode" value="preview" checked><span>${icon("search")}<strong>Preview cleanup</strong><small>See the plan before making changes.</small></span></label><label><input type="radio" name="mode" value="run"><span>${icon("layers")}<strong>Clean merged worktrees</strong><small>Review and confirm in the terminal.</small></span></label></div><label class="checkbox-row"><input type="checkbox" name="no-fetch"> Skip fetching from the remote</label><p class="form-error" role="alert"></p><div class="modal-footer"><button type="button" class="button ghost" data-action="prune">Prune stale worktrees</button><button class="button primary" type="submit">${icon("arrow")} Continue</button></div></form>`,
  );
  submitForm(dialog.querySelector("form")!, async (data) => {
    const args = ["clean"];
    if (data.get("mode") === "preview") args.push("--dry-run");
    if (data.has("no-fetch")) args.push("--no-fetch");
    await createSession({ path: String(data.get("cwd")).trim(), args });
  });
}

function openTmux(): void {
  modal(
    `${modalHeader("PICK UP WHERE YOU LEFT OFF", "Your persistent sessions")}<p class="modal-description">Attach to a tmux session, or start one for your selected worktree. Detaching keeps your work running.</p><div class="tmux-list">${state?.tmux.sessions.map((session) => `<button class="tmux-session" data-tmux="${h(session.name)}" data-path="${h(session.path)}">${icon("terminal")}<span><strong>${h(session.name)}</strong><small>${h(session.path)} · ${plural(session.windows, "window")}</small></span><span class="badge ${session.attached ? "clean" : "neutral"}">${session.attached ? "Attached" : "Detached"}</span>${icon("arrow")}</button>`).join("") || `<div class="small-empty">${icon("layers")}<h3>${state?.tmux.available ? "A fresh start awaits." : "tmux is not installed"}</h3><p>${state?.tmux.available ? "Create a persistent session for your worktree." : "Install tmux in a browser shell, then refresh to attach persistent sessions."}</p></div>`}</div><div class="modal-footer"><button class="button secondary" data-action="shell">Open a shell</button><button class="button primary" data-action="new-tmux" ${!state?.tmux.available ? "disabled" : ""}>${icon("plus")} New persistent session</button></div>`,
  );
}

interface PaletteEntry {
  title: string;
  subtitle: string;
  icon: string;
  action: () => void;
}
function openPalette(): void {
  const entries: PaletteEntry[] = [
    {
      title: "New worktree",
      subtitle: "Make room for your next idea",
      icon: "plus",
      action: () => openAdd(),
    },
    {
      title: "Open terminal",
      subtitle: contextPath(),
      icon: "terminal",
      action: () => void createSession({ path: contextPath() }),
    },
    {
      title: "All projects",
      subtitle: "Explore your entire constellation",
      icon: "globe",
      action: () => {
        projectFilter = null;
        query = "";
        $<HTMLInputElement>("#filter").value = "";
        renderWorkspace();
      },
    },
    {
      title: "Persistent tmux sessions",
      subtitle: "Resume a session from anywhere",
      icon: "layers",
      action: openTmux,
    },
    ...commandDefinitions.map((command) => ({
      title: `bonsai ${command[0]}`,
      subtitle: command[1],
      icon: "command",
      action: () => openCommand(command[0]),
    })),
    ...(state?.projects.flatMap((project) =>
      project.worktrees.map((tree) => ({
        title: branchName(tree),
        subtitle: `${project.name} · ${tree.path}`,
        icon: "branch",
        action: () => {
          projectFilter = null;
          query = "";
          $<HTMLInputElement>("#filter").value = "";
          selectTree(tree.path);
          renderWorkspace();
        },
      })),
    ) || []),
    ...[...sessions.values()].map((session) => ({
      title: session.title,
      subtitle: `Terminal · ${session.path}`,
      icon: "terminal",
      action: () => activateTerminal(session.id),
    })),
  ];
  const dialog = modal(
    `<div class="palette-search">${icon("search")}<input id="palette-input" placeholder="Where would you like to go?" aria-label="Search projects, branches, and commands" autocomplete="off"><kbd>ESC</kbd></div><div class="palette-caption" id="modal-title">PROJECTS, BRANCHES & POSSIBILITIES</div><div id="palette-results" role="listbox" aria-label="Search results"></div><div class="palette-footer"><span><kbd>↑</kbd><kbd>↓</kbd> to explore</span><span><kbd>↵</kbd> to open</span><span>Type a little. Find a lot.</span></div>`,
    "palette",
  );
  let matches = entries.slice(0, 12);
  let active = 0;
  function draw(): void {
    $("#palette-results").innerHTML =
      matches
        .map(
          (entry, index) =>
            `<button class="palette-result ${index === active ? "active" : ""}" data-palette-index="${index}" role="option" aria-selected="${index === active}"><span class="palette-icon">${icon(entry.icon)}</span><span><strong>${h(entry.title)}</strong><small>${h(entry.subtitle)}</small></span>${index === active ? '<span class="palette-enter">↵</span>' : ""}</button>`,
        )
        .join("") ||
      '<div class="small-empty">No matches. Try a branch name or command.</div>';
  }
  function choose(index: number): void {
    const entry = matches[index];
    if (entry) {
      dialog.close();
      entry.action();
    }
  }
  draw();
  const input = $<HTMLInputElement>("#palette-input");
  input.focus();
  input.addEventListener("input", () => {
    matches = entries
      .map((entry) => ({
        entry,
        score: fuzzyScore(input.value, `${entry.title} ${entry.subtitle}`),
      }))
      .filter((item) => item.score >= 0)
      .sort((a, b) => b.score - a.score)
      .slice(0, 40)
      .map((item) => item.entry);
    active = 0;
    draw();
  });
  input.addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      active =
        (active + (event.key === "ArrowDown" ? 1 : -1) + matches.length) %
        (matches.length || 1);
      draw();
      $("#palette-results .active")?.scrollIntoView({ block: "nearest" });
    }
    if (event.key === "Enter") {
      event.preventDefault();
      choose(active);
    }
  });
  $("#palette-results").addEventListener("click", (event) => {
    const row = (event.target as Element).closest<HTMLElement>(
      "[data-palette-index]",
    );
    if (row) choose(Number(row.dataset.paletteIndex));
  });
}

async function restoreSessions(): Promise<void> {
  const existing = await api<Session[]>("/api/terminals");
  let changed = false;
  for (const session of existing) {
    if (!sessions.has(session.id)) changed = true;
    sessions.set(session.id, session);
  }
  for (const id of sessions.keys())
    if (
      !existing.some((session) => session.id === id) &&
      terminals.get(id)?.status !== "exited"
    ) {
      disposeTerminal(id);
      changed = true;
    }
  if (changed) renderTerminalTabs();
}

async function createSession(options: {
  path: string;
  args?: string[];
  tmux?: string;
  newTmux?: boolean;
}): Promise<void> {
  const session = await api<Session>("/api/terminals", {
    method: "POST",
    body: JSON.stringify(options),
  });
  sessions.set(session.id, session);
  $<HTMLDialogElement>("#modal").close();
  activateTerminal(session.id);
  void refresh();
}

function activateTerminal(id: string): void {
  if (!sessions.has(id)) return;
  selectedTerminal = id;
  dockOpen = true;
  updateDock();
  let live = terminals.get(id);
  if (!live) {
    const element = document.createElement("div");
    element.className = "terminal-pane";
    element.dataset.terminalId = id;
    $("#terminal-body").append(element);
    const terminal = new Terminal({
      cursorBlink: true,
      fontFamily: '"SFMono-Regular", Consolas, "Liberation Mono", monospace',
      fontSize: 13,
      lineHeight: 1.3,
      scrollback: 10000,
      allowProposedApi: false,
      theme: {
        background: "#101411",
        foreground: "#dce2d7",
        cursor: "#bfdc8f",
        selectionBackground: "#354a37",
        black: "#202820",
        red: "#df8c7c",
        green: "#b9d78e",
        yellow: "#dec28f",
        blue: "#9dbace",
        magenta: "#c5abd6",
        cyan: "#91c7bd",
        white: "#e1e5db",
        brightBlack: "#6c7b70",
        brightRed: "#f3a193",
        brightGreen: "#d0e5ac",
        brightYellow: "#efdab0",
        brightBlue: "#bdd8ee",
        brightMagenta: "#ddc7ec",
        brightCyan: "#b6e1d9",
        brightWhite: "#ffffff",
      },
    });
    const fit = new FitAddon();
    terminal.loadAddon(fit);
    terminal.open(element);
    live = { terminal, fit, socket: null, element, status: "connecting" };
    terminals.set(id, live);
    terminal.onData((data) => {
      const socket = terminals.get(id)?.socket;
      if (socket?.readyState === WebSocket.OPEN)
        for (const chunk of terminalInputChunks(new TextEncoder().encode(data)))
          socket.send(chunk);
    });
    terminal.onBinary((data) => {
      const socket = terminals.get(id)?.socket;
      if (socket?.readyState === WebSocket.OPEN)
        for (const chunk of terminalInputChunks(
          Uint8Array.from(data, (char) => char.charCodeAt(0)),
        ))
          socket.send(chunk);
    });
    terminal.onResize(({ cols, rows }) => {
      const socket = terminals.get(id)?.socket;
      if (socket?.readyState === WebSocket.OPEN)
        socket.send(JSON.stringify({ type: "resize", cols, rows }));
    });
    connectTerminal(id);
  }
  terminals.forEach((session, sessionId) => {
    session.element.hidden = sessionId !== id;
  });
  renderTerminalTabs();
  requestAnimationFrame(() => {
    live.fit.fit();
    live.terminal.focus();
  });
}

function connectTerminal(id: string): void {
  const live = terminals.get(id)!;
  live.socket?.close();
  live.status = "connecting";
  const protocol = location.protocol === "https:" ? "wss:" : "ws:";
  const socket = new WebSocket(
    `${protocol}//${location.host}/api/terminals/${encodeURIComponent(id)}/ws?token=${encodeURIComponent(token)}`,
  );
  socket.binaryType = "arraybuffer";
  live.socket = socket;
  socket.onopen = () => {
    live.status = "live";
    live.fit.fit();
    socket.send(
      JSON.stringify({
        type: "resize",
        cols: live.terminal.cols,
        rows: live.terminal.rows,
      }),
    );
    renderTerminalTabs();
  };
  socket.onmessage = (event) => {
    if (event.data instanceof ArrayBuffer) {
      live.terminal.write(new Uint8Array(event.data));
      return;
    }
    try {
      const message = JSON.parse(event.data);
      if (message.type === "reset") live.terminal.reset();
      if (message.type === "error") {
        live.terminal.writeln(`\r\n\x1b[31m${message.message}\x1b[0m`);
        toast(message.message, true);
      }
      if (message.type === "exit") {
        live.status = "exited";
        live.terminal.writeln(
          `\r\n\x1b[90m[Process exited${message.code !== null ? ` with code ${message.code}` : ""}]\x1b[0m`,
        );
        renderTerminalTabs();
        void refresh();
      }
    } catch {
      live.terminal.write(String(event.data));
    }
  };
  socket.onclose = () => {
    if (live.socket !== socket) return;
    live.socket = null;
    if (live.status !== "exited") live.status = "offline";
    renderTerminalTabs();
  };
  socket.onerror = () => {
    if (live.status !== "exited") live.status = "offline";
    renderTerminalTabs();
  };
}

function disposeTerminal(id: string): void {
  const live = terminals.get(id);
  if (live) {
    live.socket?.close();
    live.terminal.dispose();
    live.element.remove();
    terminals.delete(id);
  }
  sessions.delete(id);
  if (selectedTerminal === id)
    selectedTerminal = sessions.keys().next().value || null;
}

function confirmCloseTerminal(id: string): void {
  const session = sessions.get(id);
  if (!session) return;
  const dialog = modal(
    `${modalHeader("TERMINAL SESSION", session.kind === "tmux" ? "Detach this browser terminal?" : "Close this terminal?")}<p class="modal-description">${session.kind === "tmux" ? "Your tmux session will keep running. You can attach again from the sessions browser." : "This ends the shell or command running in this terminal, including any foreground work."}</p><div class="form-path">${h(session.title)}</div><form><p class="form-error" role="alert"></p><div class="modal-footer"><button class="button secondary" type="button" data-action="close-modal">Keep open</button><button class="button ${session.kind === "tmux" ? "primary" : "danger"}" type="submit">${session.kind === "tmux" ? "Detach" : "Close terminal"}</button></div></form>`,
  );
  submitForm(dialog.querySelector("form")!, async () => {
    await api(`/api/terminals/${encodeURIComponent(id)}`, { method: "DELETE" });
    disposeTerminal(id);
    if (selectedTerminal) activateTerminal(selectedTerminal);
    else renderTerminalTabs();
  });
}

function renderTerminalTabs(): void {
  $("#session-count").textContent = String(sessions.size);
  $("#terminal-tabs").innerHTML = [...sessions.values()]
    .map(
      (session) =>
        `<div class="terminal-tab ${session.id === selectedTerminal ? "active" : ""}"><button role="tab" aria-selected="${session.id === selectedTerminal}" data-session="${h(session.id)}" title="${h(session.path)}"><i class="dot ${terminals.get(session.id)?.status === "live" ? "" : "muted-dot"}"></i>${h(short(session.title, 26))}${session.kind === "tmux" ? '<span class="tmux-tag">tmux</span>' : ""}</button><button class="tab-close" data-close-session="${h(session.id)}" aria-label="Close ${h(session.title)}">${icon("close")}</button></div>`,
    )
    .join("");
  const session = selectedTerminal ? sessions.get(selectedTerminal) : null;
  const live = selectedTerminal ? terminals.get(selectedTerminal) : null;
  $("#terminal-context").innerHTML = session
    ? `<span>${icon("folder")}${h(session.path)}</span><span class="terminal-connection">${live?.status === "offline" ? `<span class="amber-text">Disconnected</span><button data-action="reconnect-terminal">Reconnect</button>` : live?.status === "exited" ? `<span>Process finished</span><button data-action="shell">New shell</button>` : live?.status === "connecting" ? "Connecting…" : session.kind === "tmux" ? "Persistent tmux · Ctrl+B, D to detach" : "Live shell · full keyboard input"}</span>`
    : "";
  $(".terminal-empty")?.classList.toggle("hidden", Boolean(selectedTerminal));
}

function updateDock(): void {
  $("#terminal-dock").classList.toggle("open", dockOpen);
  $("#terminal-dock").classList.toggle("expanded", dockExpanded && dockOpen);
  document.body.classList.toggle("terminal-expanded", dockExpanded && dockOpen);
  requestAnimationFrame(() => {
    if (selectedTerminal) terminals.get(selectedTerminal)?.fit.fit();
  });
}

async function performAction(action: string): Promise<void> {
  switch (action) {
    case "palette":
      openPalette();
      break;
    case "all":
      projectFilter = null;
      query = "";
      $<HTMLInputElement>("#filter").value = "";
      renderWorkspace();
      break;
    case "clear-filter":
      query = "";
      projectFilter = null;
      $<HTMLInputElement>("#filter").value = "";
      renderWorkspace();
      break;
    case "refresh":
      await refresh();
      if (connected) toast("Your workspace is up to date.");
      break;
    case "add":
      openAdd();
      break;
    case "add-from-here":
      openAdd("HEAD");
      break;
    case "start":
      openCommand("start");
      break;
    case "remove":
      openRemove();
      break;
    case "command":
      openCommand();
      break;
    case "clean":
      openClean();
      break;
    case "prune":
      openCommand("prune", "--all");
      break;
    case "resume":
      openCommand("resume");
      break;
    case "workspace":
      openCommand("workspace");
      break;
    case "sessions":
      openTmux();
      break;
    case "shell":
      await createSession({ path: contextPath() });
      break;
    case "new-tmux":
      await createSession({ path: contextPath(), newTmux: true });
      break;
    case "close-modal":
      $<HTMLDialogElement>("#modal").close();
      break;
    case "copy-path":
      await navigator.clipboard.writeText(selectedPath);
      toast("Worktree path copied.");
      break;
    case "zoom-in":
      zoom(1.2);
      break;
    case "zoom-out":
      zoom(1 / 1.2);
      break;
    case "fit":
      fitGraph();
      break;
    case "sidebar":
      document.body.classList.toggle("sidebar-open");
      break;
    case "close-inspector":
      inspectorMobileOpen = false;
      $("#inspector").classList.remove("has-selection");
      break;
    case "toggle-dock":
      dockOpen = !dockOpen;
      updateDock();
      break;
    case "expand-dock":
      dockExpanded = !dockExpanded;
      dockOpen = true;
      updateDock();
      break;
    case "reconnect-terminal":
      if (selectedTerminal && terminals.has(selectedTerminal)) {
        terminals.get(selectedTerminal)!.terminal.reset();
        connectTerminal(selectedTerminal);
      }
      break;
  }
}

document.addEventListener("click", (event) => {
  const target = event.target as Element;
  const action = target.closest<HTMLElement>("[data-action]");
  if (action) {
    void performAction(action.dataset.action!).catch((error) =>
      toast(error instanceof Error ? error.message : String(error), true),
    );
    return;
  }
  const openShell = target.closest<HTMLElement>("[data-open-shell]");
  if (openShell) {
    void createSession({ path: openShell.dataset.openShell! }).catch((error) =>
      toast(String(error), true),
    );
    return;
  }
  const tree = target.closest<HTMLElement>("[data-tree]");
  if (tree) {
    selectTree(tree.dataset.tree!);
    return;
  }
  const project = target.closest<HTMLElement>(
    "[data-project], [data-project-node]",
  );
  if (project) {
    projectFilter =
      project.dataset.project || project.dataset.projectNode || null;
    query = "";
    $<HTMLInputElement>("#filter").value = "";
    const first = state?.projects.find((item) => item.id === projectFilter)
      ?.worktrees[0];
    if (first) selectedPath = first.path;
    document.body.classList.remove("sidebar-open");
    renderWorkspace();
    $("#inspector").scrollTop = 0;
    return;
  }
  const viewButton = target.closest<HTMLElement>("[data-view]");
  if (viewButton) {
    view = viewButton.dataset.view as "graph" | "list";
    localStorage.setItem("bonsai.view", view);
    graphKey = "";
    renderWorkspace();
    return;
  }
  const activityTerminal = target.closest<HTMLElement>(
    "[data-activity-terminal]",
  );
  if (activityTerminal) {
    const id = activityTerminal.dataset.activityTerminal!;
    void restoreSessions()
      .then(() => {
        if (sessions.has(id)) activateTerminal(id);
        else
          toast(
            "This terminal is no longer available. Refresh the workspace to update its activity.",
            true,
          );
      })
      .catch((error) =>
        toast(error instanceof Error ? error.message : String(error), true),
      );
    return;
  }
  const session = target.closest<HTMLElement>("[data-session]");
  if (session) {
    activateTerminal(session.dataset.session!);
    return;
  }
  const close = target.closest<HTMLElement>("[data-close-session]");
  if (close) {
    confirmCloseTerminal(close.dataset.closeSession!);
    return;
  }
  const tmux = target.closest<HTMLElement>("[data-tmux]");
  if (tmux)
    void createSession({
      path: tmux.dataset.path!,
      tmux: tmux.dataset.tmux!,
    }).catch((error) => toast(String(error), true));
});

$<HTMLInputElement>("#filter").addEventListener("input", (event) => {
  query = (event.target as HTMLInputElement).value;
  renderCanvas();
});
$<HTMLDialogElement>("#modal").addEventListener("click", (event) => {
  if (event.target === event.currentTarget) {
    const rect = (event.currentTarget as Element).getBoundingClientRect();
    if (
      event.clientX < rect.left ||
      event.clientX > rect.right ||
      event.clientY < rect.top ||
      event.clientY > rect.bottom
    )
      $<HTMLDialogElement>("#modal").close();
  }
});
document.addEventListener("keydown", (event) => {
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
    event.preventDefault();
    openPalette();
    return;
  }
  const target = event.target as Element;
  if (
    (event.key === "Enter" || event.key === " ") &&
    target.matches("g[data-tree], g[data-project-node]")
  ) {
    event.preventDefault();
    target.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  }
  if (
    target.closest("input, textarea, select, .xterm") ||
    $<HTMLDialogElement>("#modal").open
  )
    return;
  if (event.key === "/") {
    event.preventDefault();
    $<HTMLInputElement>("#filter").focus();
  }
  if (event.key === "Escape") {
    document.body.classList.remove("sidebar-open");
    if (dockExpanded) {
      dockExpanded = false;
      updateDock();
    } else {
      query = "";
      $<HTMLInputElement>("#filter").value = "";
      renderCanvas();
    }
  }
});
new ResizeObserver(() => {
  if (selectedTerminal) {
    const live = terminals.get(selectedTerminal);
    if (live && dockOpen) live.fit.fit();
  }
}).observe($("#terminal-body"));
new ResizeObserver(() => {
  if (graphFitted && view === "graph") fitGraph();
}).observe($("#canvas"));
window.addEventListener("online", () => void refresh());
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) void refresh();
});
void refresh();
