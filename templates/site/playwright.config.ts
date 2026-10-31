import { defineConfig, devices } from "@playwright/test";

const CI = !!process.env.CI;

export default defineConfig({
  testDir: "tests/e2e",
  outputDir: "test-results/e2e",
  fullyParallel: true,
  retries: CI ? 1 : 0,
  reporter: [["html", { outputFolder: "playwright-report", open: "never" }], ["list"]],
  use: {
    video: "on",
    trace: "on",
    viewport: { width: 1280, height: 800 },
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
  webServer: [
    {
      command: "npm run e2e:actor",
      url: "http://127.0.0.1:4190/v1/me",
      reuseExistingServer: !CI,
      timeout: 30_000,
    },
    {
      command: "npm run e2e:build:fake && npm run e2e:preview:fake",
      url: "http://127.0.0.1:4173",
      reuseExistingServer: !CI,
      timeout: 120_000,
    },
    {
      command:
        "VITE_LULA_API_URL=http://127.0.0.1:4190 npm run e2e:build:http && npm run e2e:preview:http",
      url: "http://127.0.0.1:4174",
      reuseExistingServer: !CI,
      timeout: 120_000,
    },
  ],
});
