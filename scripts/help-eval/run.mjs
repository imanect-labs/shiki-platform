// ヘルプ検索（help_search）の回答の質を、実際のモデルで数える評価。CI では回さない。
//
// 使い方（docs/help-corpus.md の「回答の評価」）:
//   CF_ACCESS_CLIENT_ID=… CF_ACCESS_CLIENT_SECRET=… \
//   node scripts/help-eval/run.mjs --base https://shiki-preview.imanect.app --runs 3 --out /tmp/help-eval.jsonl
//
// 1 問をブラウザで送り、回答が終わるまで待って、次を数える:
//   - help: help_search を呼んだか / help_first: 最初の検索が help_search か / doc: doc_search を呼んだか
//   - screen: 期待する画面へのリンクが本文にあるか（questions.json の screen。null は数えない）
//   - figs_avail: 本文で引いたヘルプ（[Hn]）に付いた図の数 / figs_text: そのうちモデルが本文に入れた数
//   - figs_shown: 画面に表示され、読み込めた図の数
//   - dangling: 図が 1 枚も出ていないのに、本文が ① などの図の番号を使っているか
// 作った会話は最後に消す（--keep で残す）。プレビューは共有環境なので、後に残さない。
import { createRequire } from "node:module";
import { readFileSync, appendFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(join(here, "../../web/package.json"));
const { chromium } = require("@playwright/test");

const { values: opt } = parseArgs({
  options: {
    base: { type: "string", default: "https://shiki-preview.imanect.app" },
    runs: { type: "string", default: "3" },
    concurrency: { type: "string", default: "3" },
    only: { type: "string" },
    out: { type: "string", default: "/tmp/help-eval.jsonl" },
    state: { type: "string", default: "/tmp/help-eval-state.json" },
    keep: { type: "boolean", default: false },
  },
});

const headers = {};
if (process.env.CF_ACCESS_CLIENT_ID) {
  headers["CF-Access-Client-Id"] = process.env.CF_ACCESS_CLIENT_ID;
  headers["CF-Access-Client-Secret"] = process.env.CF_ACCESS_CLIENT_SECRET ?? "";
}
const questions = JSON.parse(readFileSync(join(here, "questions.json"), "utf8")).filter(
  (q) => !opt.only || opt.only.split(",").includes(q.id),
);
const browser = await chromium.launch(
  process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE } : {},
);
const contextOptions = { baseURL: opt.base, locale: "ja-JP", extraHTTPHeaders: headers, viewport: { width: 1400, height: 1000 } };

// ログイン状態を作る（無ければ Keycloak のフォームを通す）。
if (!existsSync(opt.state)) {
  const ctx = await browser.newContext(contextOptions);
  const page = await ctx.newPage();
  await page.goto("/login");
  await page.locator("#username").fill(process.env.SHIKI_EVAL_USER ?? "alice");
  await page.locator("#password").fill(process.env.SHIKI_EVAL_PASS ?? "password");
  await page.locator("#kc-login").click();
  await page.waitForURL((u) => u.origin === new URL(opt.base).origin && !u.pathname.startsWith("/login"));
  await ctx.storageState({ path: opt.state });
  await ctx.close();
}

const CIRCLED = /[①-⑳]/;

async function runOne(q, run) {
  const ctx = await browser.newContext({ ...contextOptions, storageState: opt.state });
  const page = await ctx.newPage();
  const rec = { id: q.id, run, q: q.q };
  let threadId = null;
  try {
    await page.goto("/");
    await page.getByLabel("メッセージを入力").fill(q.q);
    const t0 = Date.now();
    await page.getByRole("button", { name: "送信", exact: true }).click();
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 60_000 });
    threadId = page.url().split("/c/")[1];
    rec.url = page.url();
    await page.getByRole("button", { name: /良い回答|高評価|役に立った/ }).first().waitFor({ timeout: 300_000 });
    rec.ms = Date.now() - t0;
    // 図の読み込み（代理取得）を待つ。
    await page.waitForLoadState("networkidle").catch(() => {});

    const res = await ctx.request.get(`/api/threads/${threadId}/messages`);
    const body = await res.json();
    const msgs = Array.isArray(body) ? body : body.messages;
    const blocks = msgs.at(-1).content;
    const text = blocks.filter((b) => b.type === "text").map((b) => b.text).join("\n");
    const calls = blocks.filter((b) => b.type === "tool_call").map((b) => b.name);
    const searches = calls.filter((n) => n === "help_search" || n === "doc_search");
    const help = blocks.filter((b) => b.type === "help_citation");
    const cited = new Set([...text.matchAll(/\[H(\d+)\]/g)].map((m) => Number(m[1])));
    const availUrls = new Set(help.filter((c) => cited.has(c.cite_id)).flatMap((c) => (c.images ?? []).map((i) => i.url)));
    const shown = await page
      .getByTestId("answer-image")
      .evaluateAll((els) => els.filter((e) => e.complete && e.naturalWidth > 0).length);

    Object.assign(rec, {
      calls,
      help: calls.includes("help_search"),
      help_first: searches[0] === "help_search",
      doc: calls.includes("doc_search"),
      screen: q.screen ? text.includes(`](${q.screen})`) : null,
      figs_avail: availUrls.size,
      figs_text: [...availUrls].filter((u) => text.includes(u)).length,
      figs_shown: shown,
      dangling: shown === 0 && CIRCLED.test(text),
      errors: blocks.filter((b) => b.type === "tool_result" && b.is_error).length,
      chars: text.length,
      text: text.slice(0, 3000),
    });
  } catch (e) {
    rec.error = String(e).slice(0, 300);
  } finally {
    if (threadId && !opt.keep) {
      // 状態を変える API は double-submit の CSRF（Cookie shiki_csrf と同じ値のヘッダ）を要る。
      const csrf = (await ctx.cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
      const r = await ctx.request
        .delete(`/api/threads/${threadId}`, { headers: { Origin: opt.base, "X-CSRF-Token": csrf } })
        .catch(() => null);
      rec.deleted = r?.ok() ?? false;
    }
    await ctx.close();
  }
  appendFileSync(opt.out, JSON.stringify(rec) + "\n");
  console.log(
    `${rec.id}#${run}`,
    rec.error ? `ERROR ${rec.error}` : `help=${rec.help} first=${rec.help_first} doc=${rec.doc} screen=${rec.screen} figs=${rec.figs_shown}/${rec.figs_avail}(text ${rec.figs_text}) dangling=${rec.dangling} ${Math.round(rec.ms / 1000)}s`,
  );
  return rec;
}

const jobs = [];
for (let run = 1; run <= Number(opt.runs); run++) for (const q of questions) jobs.push([q, run]);
const results = [];
await Promise.all(
  Array.from({ length: Number(opt.concurrency) }, async () => {
    while (jobs.length) {
      const [q, run] = jobs.shift();
      results.push(await runOne(q, run));
    }
  }),
);
await browser.close();

const ok = results.filter((r) => !r.error);
const rate = (f, rs = ok) => {
  const xs = rs.filter((r) => f(r) !== null);
  return `${xs.filter(f).length}/${xs.length}`;
};
const withFigs = ok.filter((r) => r.figs_avail > 0);
const ms = ok.map((r) => r.ms).sort((a, b) => a - b);
console.log("\n== 集計");
console.log("失敗した実行           ", `${results.length - ok.length}/${results.length}`);
console.log("help_search を呼んだ    ", rate((r) => r.help));
console.log("最初の検索がヘルプ      ", rate((r) => r.help_first));
console.log("doc_search も呼んだ     ", rate((r) => r.doc));
console.log("画面リンクが本文にある  ", rate((r) => r.screen));
console.log("図を出せる回で図が出た  ", rate((r) => r.figs_shown > 0, withFigs));
console.log("  うちモデルが本文に入れた", rate((r) => r.figs_text > 0, withFigs));
console.log("図なしで ① などを使った ", rate((r) => r.dangling));
console.log("所要時間 中央値 / 最大  ", `${Math.round(ms[Math.floor(ms.length / 2)] / 1000)}s / ${Math.round(ms.at(-1) / 1000)}s`);
