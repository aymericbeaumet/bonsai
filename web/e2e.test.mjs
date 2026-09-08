import { test, expect } from "@playwright/test";
import { spawn, spawnSync } from "node:child_process";
import { once } from "node:events";
import {
  mkdtemp,
  mkdir,
  writeFile,
  chmod,
  rm,
  realpath,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve, delimiter } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("../", import.meta.url));
const binary =
  process.env._BONSAI_TEST_BIN || resolve(repoRoot, "target/debug/bonsai");

async function fixture(populated) {
  const directory = await realpath(
    await mkdtemp(join(tmpdir(), "bonsai-browser-")),
  );
  const root = join(directory, "workspaces");
  const testHome = join(directory, "home");
  const fakebin = join(directory, "bin");
  // Unix socket paths have a small platform limit, so keep this separate from long macOS temp paths.
  const tmuxDirectory = await mkdtemp("/tmp/bhq-");
  await Promise.all([mkdir(testHome), mkdir(fakebin)]);
  await writeFile(join(testHome, ".gitconfig"), "");
  await writeFile(
    join(fakebin, "codex"),
    '#!/bin/sh\nprintf "FRESH_CODEX_SESSION\\n"\nprintf "%s\\n" "$PWD"\n',
  );
  await chmod(join(fakebin, "codex"), 0o755);
  const env = Object.fromEntries(
    Object.entries(process.env).filter(
      ([key]) =>
        !/^(BONSAI_|GIT_|XDG_|_BONSAI_|TMUX$|BASH_ENV$|ENV$)/.test(key),
    ),
  );
  Object.assign(env, {
    HOME: testHome,
    XDG_CONFIG_HOME: join(testHome, ".config"),
    XDG_DATA_HOME: join(testHome, ".local/share"),
    GIT_CONFIG_GLOBAL: join(testHome, ".gitconfig"),
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_AUTHOR_NAME: "Browser test",
    GIT_AUTHOR_EMAIL: "test@example.com",
    GIT_COMMITTER_NAME: "Browser test",
    GIT_COMMITTER_EMAIL: "test@example.com",
    BONSAI_ROOT: root,
    BONSAI_ADD__FETCH: "false",
    BONSAI_ADD__INSTALL: "false",
    TMUX_TMPDIR: tmuxDirectory,
    SHELL: "/bin/bash",
    PATH: fakebin + delimiter + process.env.PATH,
  });
  const run = (command, args, cwd = directory) => {
    const result = spawnSync(command, args, {
      cwd,
      env,
      encoding: "utf8",
      timeout: 15_000,
    });
    if (result.status !== 0)
      throw new Error(
        `${command} ${args.join(" ")} failed: ${result.error || result.stderr}`,
      );
    return result.stdout.trim();
  };
  const projects = new Map();
  const trees = new Map();
  async function project(name, branches = []) {
    const path = join(directory, name);
    await mkdir(path);
    run("git", ["init", "-b", "main"], path);
    await writeFile(join(path, "README.md"), `${name}\n`);
    run("git", ["add", "."], path);
    run("git", ["commit", "-m", "Initial project"], path);
    projects.set(name, path);
    for (const branch of branches)
      trees.set(branch, run(binary, ["add", branch], path));
    return path;
  }
  if (populated) {
    await project("bonsai", ["ab/browser-ui", "ab/terminal-replay"]);
    await project("canopy", ["ab/accessibility", "ab/search"]);
    await project("atlas", ["ab/documentation"]);
    await writeFile(
      join(trees.get("ab/browser-ui"), "in-progress.ts"),
      'export const status = "growing";\n',
    );
  }
  const server = spawn(binary, ["hq", "--no-open", "--port", "0"], {
    cwd: directory,
    env,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let logs = "";
  server.stderr.on("data", (data) => {
    logs += data.toString();
  });
  const url = await new Promise((resolveUrl, reject) => {
    const timer = setTimeout(
      () => reject(new Error(`HQ did not start: ${logs}`)),
      15_000,
    );
    const lines = createInterface({ input: server.stdout });
    lines.on("line", (line) => {
      if (line.startsWith("http://127.0.0.1:")) {
        clearTimeout(timer);
        lines.close();
        resolveUrl(line.trim());
      }
    });
    server.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    server.once("exit", (code) => {
      clearTimeout(timer);
      reject(new Error(`HQ exited with ${code}: ${logs}`));
    });
  });
  const parsed = new URL(url);
  const token = new URLSearchParams(parsed.hash.slice(1)).get("token");
  const state = async () => {
    const response = await fetch(`${parsed.origin}/api/state`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    expect(response.ok).toBe(true);
    return response.json();
  };
  return {
    url,
    state,
    root,
    trees,
    projects,
    project,
    run,
    tmuxAvailable:
      spawnSync("tmux", ["-V"], { env, stdio: "ignore" }).status === 0,
    async close() {
      if (server.exitCode === null) {
        server.kill("SIGTERM");
        await once(server, "exit");
      }
      spawnSync("tmux", ["kill-server"], {
        env,
        stdio: "ignore",
        timeout: 3000,
      });
      await Promise.all([
        rm(directory, { recursive: true, force: true }),
        rm(tmuxDirectory, { recursive: true, force: true }),
      ]);
    },
  };
}

async function openWorkspace(page, workspace) {
  const errors = [];
  let output = "";
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("websocket", (socket) =>
    socket.on("framereceived", ({ payload }) => {
      output +=
        typeof payload === "string" ? payload : payload.toString("utf8");
    }),
  );
  await page.goto(workspace.url);
  await expect(page.locator("#connection-text")).toHaveText("Connected");
  expect(page.url()).not.toContain("#token=");
  return {
    errors,
    waitOutput: (marker) => expect.poll(() => output).toContain(marker),
    async input(command) {
      await expect(page.locator(".terminal-connection")).toContainText(
        /Live shell|Persistent tmux/,
      );
      const terminal = page.locator(
        ".terminal-pane:visible .xterm-helper-textarea",
      );
      await expect(terminal).toBeVisible();
      await terminal.focus();
      await page.keyboard.type(command + "\n");
    },
  };
}

test("workspace graph, commands, real shell, reconnect, and persistent tmux", async ({
  page,
}) => {
  const workspace = await fixture(true);
  try {
    const nestedTmuxPath = join(
      workspace.trees.get("ab/accessibility"),
      "src",
      "nested",
    );
    if (workspace.tmuxAvailable) {
      await mkdir(nestedTmuxPath, { recursive: true });
      workspace.run("tmux", [
        "new-session",
        "-d",
        "-s",
        "native-existing",
        "-c",
        nestedTmuxPath,
      ]);
    }
    const browser = await openWorkspace(page, workspace);
    await expect(page.locator("g.tree-node")).toHaveCount(8);
    await expect(page.locator("#project-count")).toHaveText("03");
    const inactiveTree = page
      .locator("g.tree-node")
      .filter({ hasText: "ab/documentation" });
    await inactiveTree.click();
    await expect(inactiveTree.locator(".node-activity")).toHaveCount(0);
    await expect(page.locator("#inspector .runtime-empty")).toContainText(
      "No active sessions",
    );
    if (workspace.tmuxAvailable) {
      await expect(
        page
          .locator("g.tree-node")
          .filter({ hasText: "ab/accessibility" })
          .locator(".node-activity"),
      ).toContainText("tmux 1");
    }
    await page
      .locator("g.tree-node")
      .filter({ hasText: "ab/browser-ui" })
      .click();
    await expect(page.locator(".inspector-title")).toHaveText("ab/browser-ui");
    await expect(page.locator("#inspector .badge.changed")).toContainText(
      "Uncommitted changes",
    );
    await page.locator("#inspector").evaluate((element) => {
      element.scrollTop = element.scrollHeight;
    });
    await page
      .locator("g.tree-node")
      .filter({ hasText: "ab/terminal-replay" })
      .click();
    expect(
      await page.locator("#inspector").evaluate((element) => element.scrollTop),
    ).toBe(0);
    await page
      .locator("g.tree-node")
      .filter({ hasText: "ab/browser-ui" })
      .click();
    expect(
      await page
        .locator(".node-title")
        .first()
        .evaluate(
          (element) =>
            parseFloat(getComputedStyle(element).fontSize) * element.getCTM().a,
        ),
    ).toBeGreaterThanOrEqual(10.9);

    await page.locator('[data-view="list"]').click();
    await expect(page.locator(".worktree-row")).toHaveCount(8);
    if (workspace.tmuxAvailable) {
      await expect(
        page
          .locator(".worktree-row")
          .filter({ hasText: "ab/accessibility" })
          .locator(".runtime-badge.tmux"),
      ).toHaveText("tmux 1");
    }
    await page.locator("#filter").fill("terminal-replay");
    await expect(page.locator(".worktree-row")).toHaveCount(1);
    await page.locator("#filter").fill("");
    await page.locator('[data-view="graph"]').click();
    await page.keyboard.press("Control+k");
    await page.locator("#palette-input").fill("browser-ui");
    await page.keyboard.press("Enter");
    await expect(page.locator(".inspector-title")).toHaveText("ab/browser-ui");

    await page.locator('#inspector [data-action="add-from-here"]').click();
    await expect(page.locator('#add-form [name="base"]')).toHaveValue("HEAD");
    await page.keyboard.press("Escape");
    await page.locator('#inspector [data-action="remove"]').click();
    await expect(page.locator('#remove-form [name="force"]')).not.toBeChecked();
    await page.keyboard.press("Escape");

    await page.locator('#inspector [data-action="shell"]').click();
    await browser.input(
      "printf 'BROWSER_%s\\n' 'TERMINAL_OK'; bonsai list --json",
    );
    await browser.waitOutput("\r\nBROWSER_TERMINAL_OK\r\n");
    await browser.waitOutput("ab/terminal-replay");
    const shell = await page
      .locator(".terminal-tab.active [data-session]")
      .getAttribute("data-session");
    await page.locator('[data-action="refresh"]').first().click();
    await expect(
      page
        .locator("g.tree-node")
        .filter({ hasText: "ab/browser-ui" })
        .locator(".node-activity"),
    ).toContainText("terminal 1");
    await page
      .locator(`#inspector [data-activity-terminal="${shell}"]`)
      .click();
    await expect(
      page.locator(".terminal-tab.active [data-session]"),
    ).toHaveAttribute("data-session", shell);
    await page.reload();
    await expect(page.locator("#connection-text")).toHaveText("Connected");
    await page.locator(`[data-session="${shell}"]`).click();
    await expect(page.locator(".terminal-connection")).toContainText(
      "Live shell",
    );
    await browser.input("printf 'RECONNECTED_%s\\n' 'OK'");
    await browser.waitOutput("\r\nRECONNECTED_OK\r\n");

    await page.locator('#inspector [data-action="start"]').click();
    await page.locator("#command-args").fill("codex");
    await page.locator('#command-form [type="submit"]').click();
    await browser.waitOutput("FRESH_CODEX_SESSION");
    await browser.waitOutput(workspace.trees.get("ab/browser-ui"));

    await page.locator('.topbar [data-action="add"]').click();
    await page.locator('#add-form [name="branch"]').fill("ab/browser-created");
    await page.locator('#add-form [type="submit"]').click();
    await browser.waitOutput("created branch 'ab/browser-created'");
    await expect
      .poll(async () =>
        (await workspace.state()).projects
          .flatMap((project) => project.worktrees)
          .map((tree) => tree.branch),
      )
      .toContain("ab/browser-created");
    await page.locator('[data-action="refresh"]').first().click();
    await page
      .locator("g.tree-node")
      .filter({ hasText: "ab/browser-created" })
      .click();
    const createdPath = await page
      .locator("g.tree-node.selected")
      .getAttribute("data-tree");
    await page.locator('#inspector [data-action="remove"]').click();
    workspace.run("git", ["switch", "--detach"], createdPath);
    workspace.run(
      "git",
      ["switch", "ab/browser-created"],
      workspace.trees.get("ab/terminal-replay"),
    );
    await page.locator('#remove-form [type="submit"]').click();
    await browser.waitOutput("removed worktree");
    await expect
      .poll(async () =>
        (await workspace.state()).projects
          .flatMap((project) => project.worktrees)
          .map((tree) => tree.path),
      )
      .not.toContain(createdPath);
    expect(
      (await workspace.state()).projects
        .flatMap((project) => project.worktrees)
        .find((tree) => tree.path === workspace.trees.get("ab/terminal-replay"))
        ?.branch,
    ).toBe("ab/browser-created");
    workspace.run(
      "git",
      ["switch", "ab/terminal-replay"],
      workspace.trees.get("ab/terminal-replay"),
    );
    await expect
      .poll(async () =>
        (await workspace.state()).projects
          .flatMap((project) => project.worktrees)
          .map((tree) => tree.branch),
      )
      .not.toContain("ab/browser-created");
    workspace.run(
      "git",
      ["show-ref", "--verify", "refs/heads/ab/browser-created"],
      workspace.projects.get("bonsai"),
    );

    if (workspace.tmuxAvailable) {
      await page.locator('[data-action="refresh"]').first().click();
      await page
        .locator("g.tree-node")
        .filter({ hasText: "ab/browser-ui" })
        .click();
      await page.locator('#inspector [data-action="new-tmux"]').click();
      await expect(page.locator(".terminal-connection")).toContainText(
        "Persistent tmux",
      );
      await browser.input("printf 'TMUX_%s\\n' 'BROWSER_OK'");
      await browser.waitOutput("\r\nTMUX_BROWSER_OK\r\n");
      await page.locator(".terminal-tab.active [data-close-session]").click();
      await page.locator('#modal [type="submit"]').click();
      await expect
        .poll(async () => (await workspace.state()).tmux.sessions.length)
        .toBeGreaterThan(0);
      await page.locator('[data-action="refresh"]').first().click();
      await page
        .locator("g.tree-node")
        .filter({ hasText: "ab/accessibility" })
        .click();
      await page
        .locator('#terminal-dock [data-action="toggle-dock"]')
        .first()
        .click();
      await expect(
        page.locator('#inspector [data-tmux="native-existing"]'),
      ).toBeVisible();
      const activityScreenshot = test
        .info()
        .outputPath("activity-inspector.png");
      await page.screenshot({ path: activityScreenshot });
      await test.info().attach("worktree activity", {
        path: activityScreenshot,
        contentType: "image/png",
      });
      await page.locator('[data-view="list"]').click();
      await page
        .locator(".worktree-row")
        .filter({ hasText: "ab/accessibility" })
        .scrollIntoViewIfNeeded();
      const listScreenshot = test.info().outputPath("activity-list.png");
      await page.screenshot({ path: listScreenshot });
      await test
        .info()
        .attach("worktree activity list", {
          path: listScreenshot,
          contentType: "image/png",
        });
      await page.locator('[data-view="graph"]').click();
      await page.locator('#inspector [data-tmux="native-existing"]').click();
      await expect(page.locator(".terminal-connection")).toContainText(
        "Persistent tmux",
      );
      await browser.input("printf 'NESTED_%s\\n' 'RUNTIME_OK'; pwd");
      await browser.waitOutput("\r\nNESTED_RUNTIME_OK\r\n");
      await browser.waitOutput(nestedTmuxPath);
      await expect(inactiveTree).toHaveCount(1);
      await expect(inactiveTree.locator(".node-activity")).toHaveCount(0);
    }

    await page.setViewportSize({ width: 390, height: 844 });
    await page.reload();
    await expect(page.locator("#connection-text")).toHaveText("Connected");
    await expect(page.locator("#inspector")).not.toBeVisible();
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await page.locator('.statusbar [data-action="palette"]').click();
    await expect(page.locator("#palette-input")).toBeVisible();
    await page.keyboard.press("Escape");
    expect(browser.errors).toEqual([]);
  } finally {
    await workspace.close();
  }
});

test("an empty workspace can add its first existing local project", async ({
  page,
}) => {
  const workspace = await fixture(false);
  try {
    const project = await workspace.project("first-project");
    const browser = await openWorkspace(page, workspace);
    await expect(page.locator("#canvas")).toContainText(
      "A place for your next idea",
    );
    await page.locator('#canvas [data-action="add"]').click();
    await page.locator('#add-form [name="cwd"]').fill(project);
    await page.locator('#add-form [name="branch"]').fill("ab/first-idea");
    await page.locator('#add-form [type="submit"]').click();
    await browser.waitOutput("created branch 'ab/first-idea'");
    await expect
      .poll(async () => (await workspace.state()).projects.length)
      .toBe(1);
    await page.locator('[data-action="refresh"]').first().click();
    await expect(page.locator("g.tree-node")).toHaveCount(2);
    expect(browser.errors).toEqual([]);
  } finally {
    await workspace.close();
  }
});
