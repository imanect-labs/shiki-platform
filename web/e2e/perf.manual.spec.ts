import * as fs from "node:fs";
import * as path from "node:path";

import { test, type Browser, type BrowserContext, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// 主要画面の読み込み性能（転送量・Web Vitals）を実ブラウザで計測する。
///
/// **手動実行専用**（`PERF=1` が無ければ skip。CI の E2E には含めない — 値がランナー性能で
/// 揺れるので合否ゲートにはしない。`.claude/skills/pr/references/perf.md` の「C. 絶対観測」用）。
///
/// ```bash
/// # compose（keycloak + shiki-server）と本番ビルドの web が前提（`pnpm e2e` と同じ）
/// pnpm build && pnpm perf
/// # 変更前の結果と比べる（Δ 列が付く）
/// PERF_BASELINE=perf-results/<base>.json pnpm perf
/// ```
///
/// 環境変数:
/// - `PERF_THROTTLE` … `slow4g`（既定。RTT 150ms・1.6Mbps・CPU 4x 減速 = Lighthouse の mobile 相当）
///   ／ `none`（無制限。localhost では転送量が時間に現れないので、比較は slow4g で行う）
/// - `PERF_RUNS` … 1 画面あたりの計測回数（既定 3）。時間系は中央値、転送量は 1 回目を採る。
/// - `PERF_ROUTES` … カンマ区切りで計測対象を絞る（例: `/,/drive`）。
/// - `PERF_OUT` … 結果 JSON の出力先（既定 `perf-results/perf-<時刻>.json`）。
///
/// 各画面は **cold**（新しいコンテキスト＝キャッシュ空）と **warm**（同じタブでの再読込）を測る。
/// cold は初回訪問の重さ（バンドルサイズ・圧縮の効き）、warm はキャッシュヘッダの効きを表す。
const ENABLED = process.env.PERF === "1";
const THROTTLE = process.env.PERF_THROTTLE ?? "slow4g";
const RUNS = Math.max(1, Number(process.env.PERF_RUNS ?? 3));
const OUT =
  process.env.PERF_OUT ??
  path.join("perf-results", `perf-${new Date().toISOString().replace(/[:.]/g, "-")}.json`);

/// 計測対象。ID を要する画面（/c/[id] 等）はデータ依存なので含めない。
const ROUTES: { path: string; auth: boolean }[] = [
  { path: "/login", auth: false },
  { path: "/", auth: true },
  { path: "/drive", auth: true },
  { path: "/search", auth: true },
  { path: "/notes/draft", auth: true },
  { path: "/csv/draft", auth: true },
  { path: "/slides/draft", auth: true },
  { path: "/workflows", auth: true },
  { path: "/reference/genui", auth: false },
].filter((r) => !process.env.PERF_ROUTES || process.env.PERF_ROUTES.split(",").includes(r.path));

type Kind = "js" | "css" | "font" | "img" | "doc" | "other";

/// 1 回の読み込みで測った値。バイト数は転送量（圧縮後・ヘッダ込み・キャッシュヒットは 0）。
type Sample = {
  ttfbMs: number;
  fcpMs: number | null;
  lcpMs: number | null;
  dclMs: number;
  loadMs: number;
  requests: number;
  bytes: Record<Kind | "total", number>;
  /// 展開後の JS サイズ（パース・実行コストの目安）。
  jsDecodedBytes: number;
  largest: { url: string; kind: Kind; bytes: number }[];
};

type RouteResult = { path: string; cold: Sample; warm: Sample };

const results: RouteResult[] = [];
let storageState: Awaited<ReturnType<BrowserContext["storageState"]>> | undefined;

test.describe.configure({ mode: "serial" });
test.skip(!ENABLED, "PERF=1 のときだけ実行する（pnpm perf）");

test.beforeAll(async ({ browser }) => {
  test.setTimeout(120_000);
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  await loginViaKeycloak(page);
  storageState = await ctx.storageState();
  await ctx.close();
});

test.afterAll(async () => {
  if (results.length === 0) return;
  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  const doc = { throttle: THROTTLE, runs: RUNS, measuredAt: new Date().toISOString(), results };
  fs.writeFileSync(OUT, JSON.stringify(doc, null, 2));
  const baseline = process.env.PERF_BASELINE
    ? (JSON.parse(fs.readFileSync(process.env.PERF_BASELINE, "utf8")) as typeof doc)
    : undefined;
  console.log(`\n${renderTable(results, baseline?.results)}\n結果: ${OUT}`);
});

for (const route of ROUTES) {
  test(`perf ${route.path}`, async ({ browser }) => {
    test.setTimeout(60_000 + RUNS * 90_000);
    const colds: Sample[] = [];
    const warms: Sample[] = [];
    for (let i = 0; i < RUNS; i++) {
      const { cold, warm } = await measureRoute(browser, route.path, route.auth);
      colds.push(cold);
      warms.push(warm);
    }
    results.push({ path: route.path, cold: aggregate(colds), warm: aggregate(warms) });
  });
}

/// 新しいコンテキストで 1 回（cold）、同じタブで再読込して 1 回（warm）測る。
async function measureRoute(browser: Browser, route: string, auth: boolean) {
  const ctx = await browser.newContext(auth ? { storageState } : {});
  try {
    const page = await ctx.newPage();
    await applyThrottle(page);
    await page.goto(route, { waitUntil: "load" });
    const cold = await collect(page);
    await page.reload({ waitUntil: "load" });
    const warm = await collect(page);
    return { cold, warm };
  } finally {
    await ctx.close();
  }
}

async function applyThrottle(page: Page) {
  if (THROTTLE === "none") return;
  if (THROTTLE !== "slow4g") throw new Error(`未知の PERF_THROTTLE: ${THROTTLE}`);
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Network.enable");
  await cdp.send("Network.emulateNetworkConditions", {
    offline: false,
    latency: 150,
    downloadThroughput: (1.6 * 1024 * 1024) / 8,
    uploadThroughput: (750 * 1024) / 8,
  });
  await cdp.send("Emulation.setCPUThrottlingRate", { rate: 4 });
}

/// load 後に遅れて来る LCP 候補・遅延 import のチャンクを拾うため、少し待ってから読む。
async function collect(page: Page): Promise<Sample> {
  await page.waitForTimeout(1500);
  const raw = await page.evaluate(async () => {
    const lcp = await new Promise<number | null>((resolve) => {
      let last: number | null = null;
      new PerformanceObserver((list) => {
        for (const e of list.getEntries()) last = e.startTime;
      }).observe({ type: "largest-contentful-paint", buffered: true });
      setTimeout(() => resolve(last), 100);
    });
    const nav = performance.getEntriesByType("navigation")[0] as PerformanceNavigationTiming;
    const fcp = performance.getEntriesByName("first-contentful-paint")[0]?.startTime ?? null;
    const res = (performance.getEntriesByType("resource") as PerformanceResourceTiming[]).map((r) => ({
      url: r.name,
      initiator: r.initiatorType,
      transfer: r.transferSize,
      decoded: r.decodedBodySize,
    }));
    return {
      ttfb: nav.responseStart,
      dcl: nav.domContentLoadedEventEnd,
      load: nav.loadEventEnd,
      docTransfer: nav.transferSize,
      fcp,
      lcp,
      res,
    };
  });

  const bytes: Sample["bytes"] = { js: 0, css: 0, font: 0, img: 0, doc: raw.docTransfer, other: 0, total: 0 };
  let jsDecodedBytes = 0;
  const all = raw.res.map((r) => ({ url: r.url, kind: classify(r.url, r.initiator), bytes: r.transfer, decoded: r.decoded }));
  for (const r of all) {
    bytes[r.kind] += r.bytes;
    if (r.kind === "js") jsDecodedBytes += r.decoded;
  }
  bytes.total = Object.entries(bytes).reduce((s, [k, v]) => (k === "total" ? s : s + v), 0);
  const largest = all
    .filter((r) => r.bytes > 0)
    .sort((a, b) => b.bytes - a.bytes)
    .slice(0, 5)
    .map(({ url, kind, bytes: b }) => ({ url: new URL(url).pathname, kind, bytes: b }));

  return {
    ttfbMs: raw.ttfb,
    fcpMs: raw.fcp,
    lcpMs: raw.lcp,
    dclMs: raw.dcl,
    loadMs: raw.load,
    requests: raw.res.length + 1,
    bytes,
    jsDecodedBytes,
    largest,
  };
}

function classify(url: string, initiator: string): Kind {
  const p = new URL(url).pathname;
  if (/\.(m?js)$/.test(p)) return "js";
  if (/\.css$/.test(p)) return "css";
  if (/\.(woff2?|ttf|otf|eot)$/.test(p)) return "font";
  if (/\.(png|jpe?g|gif|webp|avif|svg|ico)$/.test(p) || initiator === "img") return "img";
  return "other";
}

/// 時間系は中央値、転送量・リクエスト数は決定的なので 1 回目を採る。
function aggregate(samples: Sample[]): Sample {
  const med = (xs: (number | null)[]) => {
    const v = xs.filter((x): x is number => x !== null).sort((a, b) => a - b);
    return v.length === 0 ? null : v[Math.floor(v.length / 2)];
  };
  return {
    ...samples[0],
    ttfbMs: med(samples.map((s) => s.ttfbMs)) ?? 0,
    fcpMs: med(samples.map((s) => s.fcpMs)),
    lcpMs: med(samples.map((s) => s.lcpMs)),
    dclMs: med(samples.map((s) => s.dclMs)) ?? 0,
    loadMs: med(samples.map((s) => s.loadMs)) ?? 0,
  };
}

function renderTable(rows: RouteResult[], base?: RouteResult[]): string {
  const kb = (n: number) => `${(n / 1024).toFixed(0)}KB`;
  const ms = (n: number | null) => (n === null ? "-" : `${n.toFixed(0)}ms`);
  const delta = (cur: number | null, prev: number | null | undefined, fmt: (n: number) => string) => {
    if (cur === null || prev === null || prev === undefined) return "";
    const d = cur - prev;
    return ` (${d >= 0 ? "+" : ""}${fmt(d)})`;
  };
  const head =
    "| route | cold 転送 | cold JS | JS 展開後 | font | req | FCP | LCP | load | warm 転送 | warm LCP |\n" +
    "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |";
  const lines = rows.map((r) => {
    const b = base?.find((x) => x.path === r.path);
    const c = r.cold;
    return [
      r.path,
      kb(c.bytes.total) + delta(c.bytes.total, b?.cold.bytes.total, kb),
      kb(c.bytes.js) + delta(c.bytes.js, b?.cold.bytes.js, kb),
      kb(c.jsDecodedBytes) + delta(c.jsDecodedBytes, b?.cold.jsDecodedBytes, kb),
      kb(c.bytes.font),
      String(c.requests),
      ms(c.fcpMs) + delta(c.fcpMs, b?.cold.fcpMs, ms),
      ms(c.lcpMs) + delta(c.lcpMs, b?.cold.lcpMs, ms),
      ms(c.loadMs) + delta(c.loadMs, b?.cold.loadMs, ms),
      kb(r.warm.bytes.total) + delta(r.warm.bytes.total, b?.warm.bytes.total, kb),
      ms(r.warm.lcpMs) + delta(r.warm.lcpMs, b?.warm.lcpMs, ms),
    ].join(" | ");
  });
  return [head, ...lines.map((l) => `| ${l} |`)].join("\n");
}
