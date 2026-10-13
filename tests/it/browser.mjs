// The web client's smoke tests (apps/web/e2e, Playwright) against the stack of `just it --web`: a
// browser signs in with the project manager's token, asks, is refused, disconnects, and is held to
// the page's policy, all through Caddy serving the mercury-web image. They are a scenario of the run
// like the others, so the leak checks that follow cover what the browser sent too.
//
// A browser is Playwright's own Chromium (`pnpm exec playwright install chromium`, which CI does), or
// one already on the machine: MERCURY_E2E_CHANNEL=msedge or chrome. MERCURY_E2E_BROWSERS runs the
// same tests in several (chromium, edge, firefox, webkit), one after another.

import { spawnSync } from "node:child_process";
import { join } from "node:path";

import { PAGE_URL, leaked, mask, root } from "./stack.mjs";

/**
 * Runs the tests as `token`'s owner asking `question`, which the page answers with `answer`; `more` are the
 * other people and questions the sign-off and audit tests use (MERCURY_E2E_* names and values).
 */
export function browserChecks(token, question, answer, t, outputs, more = {}) {
  const web = join(root, "apps/web");
  // Playwright's own entry point under Node: no shell, and no pnpm to find on the path
  const run = spawnSync(process.execPath, [join(web, "node_modules/@playwright/test/cli.js"), "test"], {
    cwd: web,
    env: {
      ...process.env,
      MERCURY_E2E_URL: PAGE_URL,
      MERCURY_E2E_TOKEN: token,
      MERCURY_E2E_QUESTION: question,
      MERCURY_E2E_ANSWER: answer,
      ...more,
    },
    encoding: "utf8",
    // ten minutes a browser: MERCURY_E2E_BROWSERS may name up to four (apps/web/playwright.config.ts)
    timeout: 600_000 * Math.max(1, (process.env.MERCURY_E2E_BROWSERS || "chromium").split(",").filter(Boolean).length),
    maxBuffer: 64 * 1024 * 1024,
  });
  outputs.push(run.stdout ?? "", run.stderr ?? "");
  if (run.error) return t.ok(false, `the browser tests can't be run: ${run.error.message}`);
  // what Playwright lists, test by test, whatever the outcome; the token never appears, and the mask is the backstop
  console.log(mask(`${run.stdout}${run.stderr}`.trimEnd()));
  t.eq(run.status, 0, "the browser tests' exit code");
  t.ok(leaked(`${run.stdout}${run.stderr}`) === 0, "no secret in the browser tests' output");
}
