import assert from "node:assert/strict";
import { mkdtemp, readFile, writeFile, chmod, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";

async function fixture(t) {
  const store = await mkdtemp(join(tmpdir(), "bonsai-opencode-"));
  t.after(() => rm(store, { recursive: true, force: true }));
  const executable = join(store, "bridge.cjs");
  await writeFile(executable, `#!/usr/bin/env node
const fs = require('node:fs');
let input = '';
process.stdin.on('data', chunk => input += chunk);
process.stdin.on('end', () => {
  const root = process.env._BONSAI_HQ_STORE;
  fs.writeFileSync(root + '/snapshot.json', input);
  let commands = [];
  try { commands = JSON.parse(fs.readFileSync(root + '/commands.json')); fs.unlinkSync(root + '/commands.json'); } catch {}
  process.stdout.write(JSON.stringify({commands}));
});
`);
  await chmod(executable, 0o700);
  const source = (await readFile(new URL("opencode.js", import.meta.url), "utf8"))
    .replace("__BONSAI_EXECUTABLE__", JSON.stringify(executable))
    .replace("__BONSAI_STORE__", JSON.stringify(store));
  const modulePath = join(store, "plugin.mjs");
  await writeFile(modulePath, source);
  const { BonsaiHQ } = await import(pathToFileURL(modulePath));
  const calls = [];
  const client = {
    session: {
      list: async () => ({ data: [{ id: "saved", directory: "/work", title: "Saved", time: { updated: 1000 } }] }),
      status: async () => ({ data: {} }),
      promptAsync: async (input) => { calls.push(["prompt", input]); return { data: undefined }; },
      abort: async (input) => { calls.push(["interrupt", input]); return { data: true }; },
    },
    postSessionIdPermissionsPermissionId: async (input) => { calls.push(["permission", input]); return { data: true }; },
  };
  const plugin = await BonsaiHQ({ client, directory: "/work" });
  return {
    calls, plugin,
    snapshot: async () => JSON.parse(await readFile(join(store, "snapshot.json"), "utf8")),
    command: async (command) => writeFile(join(store, "commands.json"), JSON.stringify([{ id: "command", expiresAt: Math.floor(Date.now() / 1000) + 30, sessionId: "saved", ...command }])),
    event: (type, properties) => plugin.event({ event: { type, properties } }),
  };
}

test("saved history is not promoted to a live session until a native event", async (t) => {
  const f = await fixture(t);
  const before = await f.snapshot();
  assert.equal(before.sessions[0].live, false);
  assert.equal(before.sessions[0].updatedAt, 1);
  await f.event("session.status", { sessionID: "saved", status: { type: "busy" } });
  const after = await f.snapshot();
  assert.equal(after.sessions[0].live, true);
  assert.equal(after.sessions[0].state, "running");
});

test("permission replies retain exact request identity and reject stale approvals", async (t) => {
  const f = await fixture(t);
  await f.event("permission.asked", { id: "approval-1", sessionID: "saved" });
  await f.event("session.status", { sessionID: "saved", status: { type: "busy" } });
  assert.equal((await f.snapshot()).sessions[0].state, "waiting");
  await f.command({ action: "approve", requestId: "approval-1" });
  await f.event("message.updated", { info: { sessionID: "saved", modelID: "model" } });
  assert.deepEqual(f.calls[0], ["permission", { path: { id: "saved", permissionID: "approval-1" }, body: { response: "once" } }]);
  await f.command({ action: "approve", requestId: "approval-1" });
  await f.event("message.updated", { info: { sessionID: "saved" } });
  assert.equal(f.calls.length, 1);
});

test("prompts use the acknowledged asynchronous API without changing permission mode", async (t) => {
  const f = await fixture(t);
  await f.command({ action: "prompt", text: "Continue the task" });
  await f.event("session.status", { sessionID: "saved", status: { type: "idle" } });
  assert.deepEqual(f.calls, [["prompt", { path: { id: "saved" }, body: { parts: [{ type: "text", text: "Continue the task" }] } }]]);
});

test("question tools remain waiting and require answering in OpenCode", async (t) => {
  const f = await fixture(t);
  await f.event("message.part.updated", { part: { sessionID: "saved", type: "tool", tool: "question", callID: "tool-1", state: { status: "running", input: { questions: ["private question"] } } } });
  await f.event("session.status", { sessionID: "saved", status: { type: "busy" } });
  const waiting = (await f.snapshot()).sessions[0];
  assert.equal(waiting.state, "waiting");
  assert.equal(waiting.requestKind, "question");
  assert.equal(waiting.requestId, null);
  assert.equal(waiting.capabilities.includes("prompt"), false);
  assert.equal(JSON.stringify(waiting).includes("private question"), false);
  await f.command({ action: "prompt", text: "A reply cannot resolve this tool" });
  await f.event("message.updated", { info: { sessionID: "saved" } });
  assert.equal(f.calls.length, 0);
  await f.event("message.part.updated", { part: { sessionID: "saved", type: "tool", tool: "question", callID: "tool-1", state: { status: "completed" } } });
  const resumed = (await f.snapshot()).sessions[0];
  assert.equal(resumed.state, "running");
  assert.equal(resumed.capabilities.includes("prompt"), true);
});
