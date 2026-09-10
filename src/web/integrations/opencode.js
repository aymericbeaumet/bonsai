import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";

const executable = __BONSAI_EXECUTABLE__;
const store = __BONSAI_STORE__;

export const BonsaiHQ = async ({ client, directory }) => {
  const runtimeId = randomUUID();
  const sessions = new Map();
  const permissions = new Map();
  const questions = new Map();
  const results = [];
  const permissionReply = client.postSessionIdPermissionsPermissionId ?? client.postSessionByIdPermissionsByPermissionId;
  const capabilities = [
    ...(typeof client.session.promptAsync === "function" ? ["prompt"] : []),
    ...(typeof client.session.abort === "function" ? ["interrupt"] : []),
  ];
  let exchanging = false;
  const target = process.env.TMUX_PANE
    ? { tmuxPane: process.env.TMUX_PANE }
    : process.env._BONSAI_HQ_TERMINAL_ID ? { terminalId: process.env._BONSAI_HQ_TERMINAL_ID } : null;
  const seconds = () => Math.floor(Date.now() / 1000);
  const unwrap = (response) => {
    if (response?.error) throw new Error("OpenCode rejected the operation");
    return response?.data ?? response;
  };
  const save = (session) => {
    if (!session?.id) return;
    const existing = sessions.get(session.id) ?? {};
    sessions.set(session.id, {
      ...existing,
      sessionId: session.id,
      parentId: session.parentID ?? null,
      cwd: session.directory ?? directory,
      title: session.title ?? "OpenCode",
      updatedAt: session.time?.updated ? Math.floor(session.time.updated / 1000) : existing.updatedAt ?? null,
      state: existing.state ?? "unknown",
      live: existing.live ?? false,
      capabilities: existing.capabilities ?? [...capabilities],
    });
  };
  const bridge = (payload) => new Promise((resolve, reject) => {
    const child = spawn(executable, ["__hq-event", "opencode", "snapshot"], {
      env: { ...process.env, _BONSAI_HQ_STORE: store },
      stdio: ["pipe", "pipe", "ignore"],
    });
    let output = "";
    const timer = setTimeout(() => child.kill(), 4000);
    child.on("error", reject);
    child.stdout.on("data", (bytes) => {
      output += bytes.toString();
      if (output.length > 1024 * 1024) child.kill();
    });
    child.on("close", (code) => {
      clearTimeout(timer);
      if (code !== 0) return reject(new Error("Bonsai bridge unavailable"));
      try { resolve(output.trim() ? JSON.parse(output) : {}); } catch (error) { reject(error); }
    });
    child.stdin.on("error", () => {});
    child.stdin.end(JSON.stringify(payload));
  });
  const execute = async (command) => {
    const session = sessions.get(command.sessionId);
    if (!session || command.expiresAt < seconds()) throw new Error("Session or request expired");
    const path = { id: command.sessionId };
    if (command.action === "interrupt") {
      unwrap(await client.session.abort({ path }));
    } else if (command.action === "prompt") {
      if (!command.text?.trim()) throw new Error("A prompt is required");
      if (session.state === "waiting") throw new Error("Resolve the pending request in OpenCode before sending another prompt");
      if (typeof client.session.promptAsync !== "function") throw new Error("This OpenCode version does not expose asynchronous prompting");
      unwrap(await client.session.promptAsync({ path, body: { parts: [{ type: "text", text: command.text }] } }));
    } else if (["approve", "reject"].includes(command.action)) {
      const pending = permissions.get(command.requestId);
      if (!pending || pending.sessionID !== command.sessionId || session.state !== "waiting" || session.requestId !== command.requestId) throw new Error("Permission request is no longer pending");
      if (typeof permissionReply !== "function") throw new Error("This OpenCode version does not expose permission replies");
      unwrap(await permissionReply.call(client, {
        path: { id: command.sessionId, permissionID: command.requestId },
        body: { response: command.action === "approve" ? "once" : "reject" },
      }));
      permissions.delete(command.requestId);
      session.requestId = null;
      session.state = "running";
      session.waitingReason = null;
      session.capabilities = [...capabilities];
    } else { throw new Error("Unsupported action"); }
  };
  const exchange = async () => {
    if (exchanging) return;
    exchanging = true;
    const outgoing = results.splice(0);
    try {
      const response = await bridge({ runtimeId, pid: process.pid, target, sessions: [...sessions.values()], results: outgoing });
      for (const command of response.commands ?? []) {
        try {
          await execute(command);
          results.push({ id: command.id, ok: true });
        } catch (error) {
          results.push({ id: command.id, ok: false, error: String(error.message ?? error) });
        }
      }
    } catch {
      results.unshift(...outgoing);
      if (results.length > 100) results.splice(0, results.length - 100);
    } finally { exchanging = false; }
  };
  try {
    const listed = unwrap(await client.session.list());
    for (const session of Array.isArray(listed) ? listed : []) save(session);
    if (typeof client.session.status === "function") {
      const statuses = unwrap(await client.session.status());
      for (const [id, status] of Object.entries(statuses ?? {})) {
        const session = sessions.get(id);
        if (session) {
          session.state = status.type === "idle" ? "idle" : "running";
          session.live = status.type !== "idle";
        }
      }
    }
  } catch { /* Lifecycle events still discover sessions if an initial query fails. */ }
  const timer = setInterval(exchange, 1500);
  timer.unref?.();
  await exchange();
  return {
    event: async ({ event }) => {
      const data = event.properties ?? {};
      if (event.type === "session.created" || event.type === "session.updated") save(data.info);
      if (event.type === "session.deleted") sessions.delete(data.info?.id);
      const id = data.sessionID ?? data.info?.sessionID ?? data.part?.sessionID;
      const session = sessions.get(id);
      if (!session) return;
      session.live = true;
      if (event.type === "session.status") {
        session.state = questions.has(id) || (session.requestId && permissions.has(session.requestId)) ? "waiting" : data.status?.type === "idle" ? "idle" : "running";
        if (session.state === "running") { session.waitingReason = null; session.requestId = null; }
      } else if (event.type === "session.idle") {
        session.state = "completed";
        session.waitingReason = null;
        session.requestId = null;
        questions.delete(id);
        session.capabilities = [...capabilities];
      } else if (event.type === "session.error") {
        session.state = "failed";
        session.waitingReason = data.error?.name ?? "OpenCode error";
      } else if (event.type === "permission.asked" || event.type === "permission.updated") {
        permissions.set(data.id, data);
        session.state = "waiting";
        session.waitingReason = "Permission required";
        session.requestId = data.id;
        session.requestKind = "approval";
        if (typeof permissionReply === "function") {
          session.capabilities = [...capabilities.filter((capability) => capability !== "prompt"), "approve", "reject"];
        }
      } else if (event.type === "permission.replied") {
        permissions.delete(data.requestID ?? data.permissionID ?? data.id);
        session.requestId = null;
        session.waitingReason = null;
        session.state = "running";
        session.capabilities = [...capabilities];
      } else if (event.type === "message.part.updated" && data.part?.type === "tool" && data.part.tool === "question") {
        if (data.part.state?.status === "running") {
          questions.set(id, data.part.callID);
          session.state = "waiting";
          session.waitingReason = "Answer the question in OpenCode";
          session.requestKind = "question";
          session.requestId = null;
          session.capabilities = capabilities.filter((capability) => capability !== "prompt");
        } else if (questions.get(id) === data.part.callID && ["completed", "error"].includes(data.part.state?.status)) {
          questions.delete(id);
          session.state = "running";
          session.waitingReason = null;
          session.requestKind = null;
          session.capabilities = [...capabilities];
        } else { return; }
      } else if (event.type === "message.updated") {
        session.model = data.info?.modelID ?? data.info?.model?.modelID ?? session.model;
      } else { return; }
      session.updatedAt = seconds();
      await exchange();
    },
  };
};
