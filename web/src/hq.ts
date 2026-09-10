import {
  escapeHtml as h, branchName, fuzzyScore, runtimeActivity, headquartersRows, worktreeAgents, worktreeSummary,
  priorities, priorityLabels, agentHierarchy, relativeTime,
  type Agent, type WorkspaceState, type Priority, type Worktree,
} from "./model";

export function agentRow(agent: Agent, depth = 0, breadcrumb?: string): string {
  const state = agent.stale ? "stale" : agent.state;
  const location = breadcrumb || (agent.target?.tmuxPane
    ? `${agent.target.tmuxSession || "tmux"} / ${agent.target.tmuxPane}`
    : agent.target?.terminalId ? "HQ terminal" : "saved session");
  return `<button class="agent-row state-${h(state)}" data-agent="${h(agent.id)}" data-nav="agent:${h(agent.id)}" style="--depth:${Math.min(depth, 6)}" title="${h(agent.waitingReason || agent.title)}">
    <span class="session-indent">${depth ? "└" : "·"}</span><span class="provider provider-${h(agent.provider)}">${h(agent.provider)}</span><span class="agent-title">${h(agent.title || agent.sessionId || "Untitled session")}${depth ? '<small>child</small>' : ""}</span><span class="agent-state">${h(state === "waiting" ? "needs you" : state)}</span><span class="agent-model">${h(agent.model || "model unknown")}</span><span class="agent-location">${h(location)}</span><time>${relativeTime(agent.updatedAt)}</time><span aria-hidden="true">↗</span>
  </button>`;
}

function runtimeRows(state: WorkspaceState, tree: Worktree): string {
  const agents = worktreeAgents(state, tree);
  const activity = runtimeActivity(tree);
  const terminals = activity.terminals.filter((terminal) => !agents.some((agent) => agent.target?.terminalId === terminal.id));
  const panes = activity.panes.filter((pane) => !agents.some((agent) => agent.target?.tmuxPane === pane.pane && agent.target?.tmuxSession === pane.session));
  return agentHierarchy(agents).map(({ agent, depth }) => {
    const pane = activity.panes.find((pane) => pane.pane === agent.target?.tmuxPane && pane.session === agent.target?.tmuxSession);
    return agentRow(agent, depth, pane ? `${pane.session} / ${pane.windowName || pane.window} / ${pane.pane}` : undefined);
  }).join("")
    + terminals.map((terminal) => `<button class="runtime-compact" data-activity-terminal="${h(terminal.id)}" data-nav="terminal:${h(terminal.id)}"><span>›_</span><strong>${h(terminal.title)}</strong><span>HQ ${h(terminal.kind)}</span><span>Attach ↗</span></button>`).join("")
    + panes.map((pane) => `<button class="runtime-compact" data-tmux="${h(pane.session)}" data-tmux-pane="${h(pane.pane)}" data-path="${h(tree.path)}" data-nav="pane:${h(pane.pane)}"><span>▱</span><strong>${h(pane.command)}</strong><span>${h(pane.session)} / ${h(pane.windowName || pane.window)} / ${h(pane.pane)}</span><span>Attach ↗</span></button>`).join("")
    || '<p class="inline-empty">No active sessions. Open details to start or resume one.</p>';
}

export function renderHeadquarters(state: WorkspaceState, query: string, projectId: string | null, selected: string, expanded: Set<string>, collapsed: Set<Priority>): string {
  const rows = headquartersRows(state, query, projectId);
  const groups = priorities.map((priority) => {
    const group = rows.filter((row) => row.priority === priority);
    if (!group.length) return "";
    const closed = !query && collapsed.has(priority);
    return `<section class="hq-group priority-${priority}"><button class="group-heading" data-group="${priority}" aria-expanded="${!closed}"><span>${closed ? "▸" : "▾"} ${priorityLabels[priority]}</span><span>${group.length}</span>${priority === "older" ? '<small>Kept out of your way. Always searchable.</small>' : ""}</button>${closed ? "" : group.map(({ project, worktree: tree }) => {
      const agents = worktreeAgents(state, tree);
      const activity = runtimeActivity(tree);
      const open = expanded.has(tree.path) || Boolean(query && agents.length);
      const providers = [...new Set(agents.map((agent) => agent.provider))].join(" · ");
      const summary = worktreeSummary(state, tree);
      const waiting = agents.filter((agent) => agent.state === "waiting" && !agent.stale).length;
      return `<div class="worktree-entry"><div class="worktree-line"><button class="worktree-row ${selected === tree.path ? "selected" : ""}" data-tree="${h(tree.path)}" data-nav="tree:${h(tree.path)}" aria-expanded="${open}" title="${h(tree.path)}"><span class="row-chevron">${open ? "▾" : "▸"}</span><span class="row-project">${h(project.name)}</span><span class="row-identity"><strong class="row-branch">${h(branchName(tree))}</strong>${summary ? `<span class="row-task" title="${h(summary.waitingReason || summary.title)}">${h(summary.title)}</span>` : ""}</span><span class="row-providers">${h(providers || (activity.panes.length ? "tmux" : ""))}</span><span class="row-session-count">${agents.length ? `${agents.length} session${agents.length === 1 ? "" : "s"}` : activity.summary || "—"}</span><span class="row-attention">${waiting ? `${waiting} need you` : ""}</span><span class="row-git ${tree.dirty ? "changed" : ""}">${tree.prunable ? "missing" : tree.dirty === null ? "git unknown" : tree.dirty ? `+${tree.added + tree.untracked} ~${tree.modified} −${tree.deleted}` : "clean"}</span><time>${relativeTime(tree.lastActivity)}</time></button><button class="row-details" data-details="${h(tree.path)}" aria-label="Details for ${h(branchName(tree))}" title="Worktree actions and details">•••</button></div>${open ? `<div class="worktree-sessions">${runtimeRows(state, tree)}</div>` : ""}</div>`;
    }).join("")}</section>`;
  }).join("");
  const unmatched = (state.agents || []).filter((agent) => !state.projects.some((project) => project.worktrees.some((tree) => tree.path === agent.worktreePath)) && fuzzyScore(query, `${agent.provider} ${agent.title} ${agent.model} ${agent.cwd} ${agent.target?.tmuxPane}`) >= 0);
  return `<div class="hq-list" aria-label="Worktrees and coding sessions"><div class="hq-columns"><span>PROJECT / WORKTREE</span><span>SESSIONS</span><span>GIT</span><span>ACTIVE</span></div>${groups || '<div class="hq-empty"><h2>No matching worktrees</h2><p>Search a project, branch, agent, model or tmux pane.</p><button class="button secondary" data-action="clear-filter">Clear filters</button></div>'}${!projectId && unmatched.length ? `<section class="hq-group"><div class="group-heading"><span>Outside known worktrees</span><span>${unmatched.length}</span></div>${agentHierarchy(unmatched).map(({ agent, depth }) => agentRow(agent, depth)).join("")}</section>` : ""}</div>`;
}

export function quotaSummary(state: WorkspaceState): string {
  return ["claude", "codex", "opencode"].map((provider) => {
    const quotas = (state.quotas || []).filter((quota) => quota.provider === provider);
    const known = quotas.filter((quota) => quota.usedPercent !== null);
    const highest = known.reduce((max, quota) => Math.max(max, quota.usedPercent || 0), 0);
    const stale = known.some((quota) => quota.stale);
    return `<button class="quota-chip ${highest >= 90 ? "quota-low" : ""}" data-action="quotas" title="${h(provider)} subscription quotas"><span class="provider provider-${provider}">${provider}</span><span>${known.length ? `${Math.round(highest)}% used${stale ? " · stale" : ""}` : "quota unavailable"}</span>${known.length ? `<i style="--used:${Math.min(100, Math.max(0, highest))}%"></i>` : ""}</button>`;
  }).join("");
}
