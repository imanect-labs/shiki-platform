import base from "./playwright.config";
export default {
  ...base,
  use: {
    ...base.use,
    launchOptions: { executablePath: "/opt/pw-browsers/chromium" },
    screenshot: "only-on-failure",
  },
  projects: base.projects?.map((p) => ({ ...p, use: { ...p.use, launchOptions: { executablePath: "/opt/pw-browsers/chromium" } } })),
  outputDir: "/tmp/claude-0/-home-user-shiki-platform/764cd086-21c7-59da-896e-6791ee76d106/scratchpad/pw-out",
};
