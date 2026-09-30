import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests-browser",
  timeout: 60000,
  expect: { timeout: 5000 },
  fullyParallel: true,
  workers: 2,
  retries: 0,
  reporter: [["list"], ["html", { open: "never" }]],
  use: {
    baseURL: "http://127.0.0.1:1420",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  projects: ["ko", "en"].flatMap((lang) =>
    ["light", "dark"].flatMap((theme) => [
      {
        name: `${lang}-${theme}-desktop`,
        use: {
          browserName: "chromium" as const,
          locale: lang,
          colorScheme: theme as "light" | "dark",
          viewport: { width: 680, height: 820 },
        },
      },
      {
        name: `${lang}-${theme}-compact`,
        use: {
          browserName: "chromium" as const,
          locale: lang,
          colorScheme: theme as "light" | "dark",
          viewport: { width: 420, height: 620 },
        },
      },
    ]),
  ),
  webServer: {
    command: "npm run dev -- --host 127.0.0.1",
    url: "http://127.0.0.1:1420",
    reuseExistingServer: !process.env.CI,
  },
});
