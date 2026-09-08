import { escapeHtml as h, runtimeActivity, type Worktree } from "./model";

export function activityBadges(worktree: Worktree): string {
  const { labels } = runtimeActivity(worktree);
  return labels.length
    ? `<span class="list-activity">${labels.map((label) => `<span class="runtime-badge ${label.kind}">${h(label.text)}</span>`).join("")}</span>`
    : "";
}

export function inspectorActivity(worktree: Worktree): string {
  const activity = runtimeActivity(worktree);
  const terminals = activity.terminals
    .map(
      (terminal) =>
        `<button class="runtime-row" data-activity-terminal="${h(terminal.id)}" title="Attach to ${h(terminal.title)}"><span class="runtime-mark">›_</span><span><strong>${h(terminal.title)}</strong><small>HQ terminal · ${h(terminal.kind)}</small></span><span class="runtime-attach">Attach ↗</span></button>`,
    )
    .join("");
  const tmux = activity.sessions
    .map(
      (session) =>
        `<div class="runtime-session"><button class="runtime-row" data-tmux="${h(session.session)}" data-path="${h(worktree.path)}" title="Attach to tmux session ${h(session.session)}"><span class="runtime-mark tmux">▱</span><span><strong>${h(session.session)}</strong><small>tmux · ${session.panes.length} ${session.panes.length === 1 ? "pane" : "panes"}</small></span><span class="runtime-attach">Attach ↗</span></button><ul class="runtime-panes">${session.panes.map((pane) => `<li title="Window ${h(pane.window)}, pane ${h(pane.pane)}${pane.active ? ", selected pane" : ""}"><span class="dot ${pane.active ? "" : "muted-dot"}"></span><span>${h(pane.windowName || pane.windowIndex)} · ${h(pane.pane)}</span><code>${h(pane.command)}</code></li>`).join("")}</ul></div>`,
    )
    .join("");
  return `<div class="inspector-section runtime-activity"><div class="section-caption">LIVE ACTIVITY</div>${terminals || tmux ? terminals + tmux : '<p class="runtime-empty">No active sessions.<span>Open a terminal whenever you’re ready.</span></p>'}</div>`;
}
