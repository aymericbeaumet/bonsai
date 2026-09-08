import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: ".",
  testMatch: "e2e.test.mjs",
  fullyParallel: false,
  workers: 1,
  forbidOnly: Boolean(process.env.CI),
  timeout: 60_000,
  expect: { timeout: 12_000 },
  use: {
    browserName: "chromium",
    channel: process.env._BONSAI_BROWSER_CHANNEL || undefined,
    viewport: { width: 1600, height: 1050 },
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
  },
});
