import type { Json } from "../http";
import { addDays, lastWeekStart } from "../time";
import type { GrantDef, WorldDef } from "../world";

/// APP-08 週次 KPI ダッシュボード（経営企画・台帳 §7・§7.13）の世界。
///
/// - 人: alice は経営企画（spec ごとの部署 `{{role.keiei}}`）で、表・ワークフロー・アプリの持ち主・週次 KPI の有効化者
///   （委譲元）。商談は全部読める。bob は営業で、自分が担当の商談だけを読め、ダッシュボードを見る人。
/// - 表: 商談（担当者か経営企画だけが読み書きできる・集計の最小件数 K は既定の 5 のまま・地域の少人数を伏せる）・
///   週次 KPI（「{週の初め}|{地域}」の重複しない列・伏せた地域は数を空にし、状態の列に「件数が少ないため非表示」と書く。
///   真偽の型が無いので文字の列で代える・GAP-25）・週次報告（週の初めの重複しない列・AI のコメントと .md）。
/// - ワークフロー:
///   - 週次 KPI（毎週月曜 7 時・Asia/Tokyo）: 先週・前々週（transform.date）→ 受注の地域ごとの合計と件数・決着
///     （受注＋失注）の件数・前週の受注（data.query の集計）→ script（KPI の行と表。伏せたかは「件数が null か」で決め、
///     伏せた値から率や前週比を作らない）→ data.bulk_upsert（match_on 集計のキー・流し直しても 2 行にならない）→
///     llm.invoke（コメント・失敗しても止めない）→ script（本文）→ storage.write（KPI-<週の初め>.md・同じ名前は保存
///     しない）→ 週次報告に upsert。KPI は LLM より前に書く。LLM の出力が流れる先は本文とコメントの列だけ。
///     interactive のきっかけは付けない（押した人の見え方で数え直して上書きしてしまう・計画の risk）。
///   - 週次 KPI の写し（`weeklyIr` の引数で作る・本番の IR には駆動語を入れない）: コメントの LLM を失敗させる版
///     （slow:30 ＋ 5 秒の時間切れ・形の指定つきの refuse:・形の指定なしの fail:）。
///   - 最新の週を見る（ボタン）: 週次 KPI の最新の週を読み、地域別の受注額と前週比の表を返す（押した人の一時的な
///     表示だけ）。伏せた地域は注記にする。読むのは週次 KPI の表だけなので、押す bob に商談の権限は要らない。グラフ
///     （```ui bar）で返す GAP-26 の回避策は、workflow.reply が図を描かないので使えない（findings）。
/// - アプリ: 「週次 KPI」（週次 KPI の表と、最新の週のボタン）と、集計のビューを表に束ねた「受注の集計（ビュー）」（F6・
///   集計のビューは表に出せないと伝えることの確かめ）。
/// 架空の名前だけを使う（public リポジトリ）。

export type Region = "東" | "西" | "北" | "南";
export type Stage = "リード" | "提案" | "交渉" | "受注" | "失注";
export type Deal = { name: string; region: Region; stage: Stage; amount: number; close_date: string; sales: "alice" | "bob" };

/// 週と予定の日時。どれも過去の固定の週にして、本物の cron が数える「先週」（いつも 2026-11-02 以降）と重ならない
/// ようにする（同じ名前の .md は `on_name_taken: fail` でぶつかる）。台帳 §7.13 の例は 2026-11-09 の実行（11-02 の週）
/// だが、2026-11-09〜15 に流すと本物の cron の週と同じになるので、3 週前にずらした（境界の決め方は同じ・APP-03 と同じ判断）。
export const WEEK = {
  /// 本番の IR で集計を確かめる週（10-12 月 〜 10-18 日）。前週は 10-05 〜 10-11。
  main: { at: "2026-10-19T07:00", start: "2026-10-12", end: "2026-10-18", prevStart: "2026-10-05", prevEnd: "2026-10-11" },
  /// 同時に 2 本を始める週と、コメントの LLM を失敗させる写しの週（10-05 〜 10-11）。前週は 09-28 〜 10-04。
  race: { at: "2026-10-12T07:00", start: "2026-10-05", end: "2026-10-11", prevStart: "2026-09-28", prevEnd: "2026-10-04" },
} as const;

/// 伏せた地域の状態の列の値。
export const HIDDEN = "件数が少ないため非表示";

/// 北の受注の生の額（件数が少ないので、KPI・.md・reply のどこにも出てはいけない）。
export const NORTH_RAW = [4321000, 1234000];

const d = (name: string, region: Region, stage: Stage, amount: number, close_date: string, sales: "alice" | "bob"): Deal => ({
  name,
  region,
  stage,
  amount,
  close_date,
  sales,
});

/// 商談の fixture（本番の週・その前週・範囲の外）。
export const DEALS: Deal[] = [
  // 本番の週（10-12 〜 10-18）: 東 受注 6・失注 2、西 受注 5・失注 1、北 受注 2・失注 1、南 は決着なし。
  d("東-本-1", "東", "受注", 300000, "2026-10-12", "bob"),
  d("東-本-2", "東", "受注", 450000, "2026-10-13", "alice"),
  d("東-本-3", "東", "受注", 120000, "2026-10-14", "bob"),
  d("東-本-4", "東", "受注", 800000, "2026-10-15", "alice"),
  d("東-本-5", "東", "受注", 230000, "2026-10-16", "bob"),
  d("東-本-6", "東", "受注", 100000, "2026-10-18", "alice"),
  d("東-本-失1", "東", "失注", 200000, "2026-10-13", "bob"),
  d("東-本-失2", "東", "失注", 150000, "2026-10-17", "alice"),
  d("東-本-リード", "東", "リード", 999000, "2026-10-15", "alice"),
  d("西-本-1", "西", "受注", 500000, "2026-10-12", "alice"),
  d("西-本-2", "西", "受注", 250000, "2026-10-14", "bob"),
  d("西-本-3", "西", "受注", 150000, "2026-10-15", "alice"),
  d("西-本-4", "西", "受注", 350000, "2026-10-16", "bob"),
  d("西-本-5", "西", "受注", 250000, "2026-10-17", "alice"),
  d("西-本-失1", "西", "失注", 300000, "2026-10-15", "bob"),
  d("北-本-1", "北", "受注", NORTH_RAW[0], "2026-10-13", "bob"),
  d("北-本-2", "北", "受注", NORTH_RAW[1], "2026-10-16", "alice"),
  d("北-本-失1", "北", "失注", 500000, "2026-10-14", "bob"),
  d("南-本-提案", "南", "提案", 700000, "2026-10-14", "alice"),
  d("南-本-交渉", "南", "交渉", 400000, "2026-10-15", "bob"),
  // 前週（10-05 〜 10-11）: 東 受注 5、西 受注 6・失注 1、北 受注 1。日曜 10-11 はこの週に入る。
  d("東-前-1", "東", "受注", 400000, "2026-10-05", "alice"),
  d("東-前-2", "東", "受注", 300000, "2026-10-06", "bob"),
  d("東-前-3", "東", "受注", 200000, "2026-10-08", "alice"),
  d("東-前-4", "東", "受注", 500000, "2026-10-09", "bob"),
  d("東-前-5", "東", "受注", 200000, "2026-10-11", "alice"),
  d("西-前-1", "西", "受注", 300000, "2026-10-05", "bob"),
  d("西-前-2", "西", "受注", 400000, "2026-10-06", "alice"),
  d("西-前-3", "西", "受注", 350000, "2026-10-07", "bob"),
  d("西-前-4", "西", "受注", 250000, "2026-10-08", "alice"),
  d("西-前-5", "西", "受注", 450000, "2026-10-09", "bob"),
  d("西-前-6", "西", "受注", 250000, "2026-10-10", "alice"),
  d("西-前-失1", "西", "失注", 100000, "2026-10-07", "alice"),
  d("北-前-1", "北", "受注", 900000, "2026-10-08", "bob"),
  // 範囲の外の大きな額: 本番の週の次の月曜と、前週の前の日曜。
  d("東-外-月曜", "東", "受注", 9990000, "2026-10-19", "alice"),
  d("西-外-日曜", "西", "受注", 8880000, "2026-10-04", "bob"),
];

/// 1 回目と 2 回目の間に足す受注（本番の週の水曜・西）。
export const LATE_DEAL: Deal = d("西-本-追加", "西", "受注", 200000, "2026-10-14", "bob");

/// 本物の cron が数える「先週」（今日の前の週の月曜〜日曜）とその前週に置く商談（今日から決める・cron の run の step
/// 出力で週を確かめ、その週に入る行だけで期待値を計算する）。東の受注 5 件のうち 2 件は bob の担当（委譲元 alice の
/// 見え方に bob の商談も入ることを、伏せられずに 5 件と数えることで確かめる）。
export function cronDeals(today: string): Deal[] {
  const monday = lastWeekStart(today);
  const day = (n: number) => addDays(monday, n);
  return [
    d("東-cron-1", "東", "受注", 210000, day(0), "alice"),
    d("東-cron-2", "東", "受注", 220000, day(1), "bob"),
    d("東-cron-3", "東", "受注", 230000, day(2), "alice"),
    d("東-cron-4", "東", "受注", 240000, day(3), "bob"),
    d("東-cron-5", "東", "受注", 250000, day(6), "alice"),
    d("東-cron-失1", "東", "失注", 300000, day(4), "bob"),
    d("西-cron-1", "西", "受注", 610000, day(1), "alice"),
    d("西-cron-2", "西", "受注", 620000, day(2), "bob"),
    d("東-cron-前-1", "東", "受注", 100000, day(-7), "alice"),
    d("東-cron-前-2", "東", "受注", 100000, day(-6), "bob"),
    d("東-cron-前-3", "東", "受注", 100000, day(-5), "alice"),
    d("東-cron-前-4", "東", "受注", 100000, day(-4), "bob"),
    d("東-cron-前-5", "東", "受注", 100000, day(-1), "alice"),
  ];
}

/// [`cronDeals`] の商談表の行（世界の定義の行と同じ形）。spec が毎分の cron の確かめの直前に置く（組んだ時の今日で
/// 置くと、spec の途中で週が替わったときに cron の週とずれる）。
export function cronDealRows(today: string): Json[] {
  return cronDeals(today).map(dealRow);
}

/// 小数 1 桁に丸める（script と同じ）。
export function round1(x: number): number {
  return Math.round(x * 10) / 10;
}

export type Kpi = {
  region: string;
  won_amount: number | null;
  won_count: number | null;
  closed_count: number | null;
  win_rate: number | null;
  wow_pct: number | null;
  status: string | null;
};

/// 集計の最小件数（既定の K）。
export const K = 5;

/// 手集計: `start`〜`end` の地域ごとの KPI（決着の無い地域は行にしない）。件数が K 未満なら伏せる（数は null）。
export function kpiOf(deals: Deal[], w: { start: string; end: string; prevStart: string; prevEnd: string }): Record<string, Kpi> {
  const inRange = (x: Deal, a: string, b: string) => x.close_date >= a && x.close_date <= b;
  const out: Record<string, Kpi> = {};
  const regions = new Set(deals.filter((x) => inRange(x, w.start, w.end) && (x.stage === "受注" || x.stage === "失注")).map((x) => x.region));
  for (const r of regions) {
    const won = deals.filter((x) => x.region === r && x.stage === "受注" && inRange(x, w.start, w.end));
    const closed = deals.filter((x) => x.region === r && (x.stage === "受注" || x.stage === "失注") && inRange(x, w.start, w.end));
    const prev = deals.filter((x) => x.region === r && x.stage === "受注" && inRange(x, w.prevStart, w.prevEnd));
    const hidden = (won.length > 0 && won.length < K) || closed.length < K;
    if (hidden) {
      out[r] = { region: r, won_amount: null, won_count: null, closed_count: null, win_rate: null, wow_pct: null, status: HIDDEN };
      continue;
    }
    const amount = won.reduce((s, x) => s + x.amount, 0);
    const prevAmount = prev.length >= K ? prev.reduce((s, x) => s + x.amount, 0) : null;
    out[r] = {
      region: r,
      won_amount: amount,
      won_count: won.length,
      closed_count: closed.length,
      win_rate: closed.length ? round1((won.length / closed.length) * 100) : null,
      wow_pct: prevAmount ? round1(((amount - prevAmount) / prevAmount) * 100) : null,
      status: null,
    };
  }
  return out;
}

const OWN = { field_cmp: { field: "sales", op: "eq", value: "user_id" } };
const KEIEI = { has_role: { role: "{{role.keiei}}", subtree: true } };

const dealRow = (x: Deal): Json => ({ ...x, sales: `{{user.${x.sales}}}` });

/// KPI の行と表を組む script（数は data.query の集計から決め、LLM に作らせない）。伏せたかは「件数が null か」で決め
/// （件数は伏せたとき以外 null にならない）、伏せた値から率・前週比を作らない。地域が空の行は「(未設定)」にしてキーの
/// 重複を避ける。$template に入れる文はどれも null にしない。
const COMPUTE_SCRIPT = `function main(i) {
  var by = {};
  var order = [];
  function slot(r) {
    var k = r === null || r === undefined || r === "" ? "(未設定)" : String(r);
    if (!by[k]) { by[k] = { region: k }; order.push(k); }
    return by[k];
  }
  (i.won || []).forEach(function (g) { slot(g.key.region).won = g.values; });
  (i.closed || []).forEach(function (g) { slot(g.key.region).closed = g.values; });
  var prev = {};
  (i.prev || []).forEach(function (g) { prev[g.key.region === null ? "(未設定)" : String(g.key.region)] = g.values; });
  order.sort();
  var rows = [];
  var lines = ["| 地域 | 受注額 | 受注件数 | 決着件数 | 受注率 | 前週比 |", "| --- | ---: | ---: | ---: | ---: | ---: |"];
  var hidden = [];
  order.forEach(function (k) {
    var s = by[k];
    var w = s.won || [0, 0];
    var c = s.closed || [0];
    var data;
    if (w[1] === null || c[0] === null) {
      hidden.push(k);
      data = { snapshot_key: i.start + "|" + k, week_start: i.start, region: k, won_amount: null, won_count: null,
        closed_count: null, win_rate: null, wow_pct: null, status: ${JSON.stringify(HIDDEN)} };
      lines.push("| " + k + " | — | — | — | — | — |");
    } else {
      var amount = Number(w[0] || 0);
      var won = Number(w[1] || 0);
      var closed = Number(c[0] || 0);
      var p = prev[k];
      var prevAmount = p && p[1] !== null && p[0] !== null ? Number(p[0]) : null;
      var rate = closed ? round1(won / closed * 100) : null;
      var wow = prevAmount ? round1((amount - prevAmount) / prevAmount * 100) : null;
      data = { snapshot_key: i.start + "|" + k, week_start: i.start, region: k, won_amount: amount, won_count: won,
        closed_count: closed, win_rate: rate, wow_pct: wow, status: null };
      lines.push("| " + k + " | " + yen(amount) + " | " + won + " | " + closed + " | " + pct(rate) + " | " + pct(wow) + " |");
    }
    rows.push({ data: data });
  });
  var table = rows.length ? lines.join("\\n") : "先週の決着した商談はありません。";
  if (hidden.length) table += "\\n\\n件数が少ない地域（" + hidden.join("、") + "）は、個人が分からないよう数を伏せています（—）。";
  return { rows: rows, count: rows.length, table_md: table };
}
function round1(x) { return Math.round(x * 10) / 10; }
function pct(x) { return x === null ? "—" : String(x) + "%"; }
function yen(x) { return String(Math.round(x)).replace(/\\B(?=(\\d{3})+(?!\\d))/g, ",") + " 円"; }`;

/// 本文（AI の失敗・打ち切りは本文に書く・分岐は script に置く）。
const BODY_SCRIPT = `function main(i) {
  var why = { step_timeout: "時間内に返らなかった", llm_refused: "AI が答えを断った", llm_output_truncated: "AI の答えが長さの上限で切れた" };
  var ai;
  var state;
  if (i.err) { ai = "（AI のコメントを作れませんでした。理由: " + (why[i.err] || "AI を呼び出せなかった") + "）"; state = "failed"; }
  else if (i.stop === "max_tokens") { ai = (i.text || "") + "\\n\\n（AI のコメントは長さの上限で途中までです）"; state = "cut"; }
  else { ai = i.text || "（AI のコメントはありません）"; state = "ok"; }
  return { md: "# 週次 KPI（" + i.start + " 〜 " + i.end + "）\\n\\n## 地域別\\n\\n" + i.table_md + "\\n\\n## コメント（AI）\\n\\n" + ai + "\\n", comment: ai, state: state };
}`;

/// コメントの失敗の code → 本文に書く理由（BODY_SCRIPT と同じ）。
export const WHY: Record<string, string> = {
  step_timeout: "時間内に返らなかった",
  llm_refused: "AI が答えを断った",
  llm_output_truncated: "AI の答えが長さの上限で切れた",
};
export const WHY_OTHER = "AI を呼び出せなかった";

const SYSTEM =
  "あなたは経営企画の担当です。地域別の週次 KPI の表を読み、目立つ動きを 3 行で短くコメントしてください。" +
  "数は表のとおりに書き、空欄（—）は件数が少ないため伏せた値なので推測しないでください。";

const lw = (path: string) => ({ $from: "nodes.last_week.output", path });
const pw = (path: string) => ({ $from: "nodes.prev_week.output", path });
const compute = (path: string) => ({ $from: "nodes.compute.output", path });
const body = (path: string) => ({ $from: "nodes.body.output", path });

function wonQuery(id: string, label: string, week: (p: string) => Json): Json {
  return {
    id,
    type: "data.query",
    label,
    params: {
      table: "{{table.deals}}",
      query: {
        filter: {
          all: [
            { cmp: { field: "stage", op: "eq", value: "受注" } },
            { cmp: { field: "close_date", op: "gte", value: week("/start") } },
            { cmp: { field: "close_date", op: "lte", value: week("/end") } },
          ],
        },
        group_by: ["region"],
        aggregate: [{ metric: "sum", field: "amount" }, { metric: "count" }],
        group_limit: 50,
      },
    },
  };
}

export type WeeklyOpts = {
  name: string;
  /// コメントの prompt の頭に置く駆動語（写しだけ）。
  prefix?: string;
  timeoutSec?: number;
  /// コメントに出力の形を付ける。
  shaped?: boolean;
};

/// 週次 KPI の IR（本番と写しで共通）。
export function weeklyIr(opts: WeeklyOpts): Json {
  const comment: Json = {
    id: "comment",
    type: "llm.invoke",
    label: "AI のコメント",
    on_error: "continue",
    params: {
      model: "{{model.chat}}",
      system: SYSTEM,
      prompt: {
        $template: `${opts.prefix ?? ""}{start} 〜 {end} の地域別の週次 KPI\n\n{table}`,
        vars: { start: lw("/start"), end: lw("/end"), table: compute("/table_md") },
      },
      max_tokens: 600,
    },
  };
  if (opts.timeoutSec) comment.timeout_sec = opts.timeoutSec;
  if (opts.shaped) {
    (comment.params as Json).output_schema = {
      type: "object",
      properties: { comment: { type: "string", description: "週次 KPI へのコメント" } },
      required: ["comment"],
      additionalProperties: false,
    };
  }
  const report = (id: string, withFile: boolean): Json => ({
    id,
    type: "data.record.upsert",
    label: withFile ? "週次報告を記録" : "週次報告を記録（ファイルなし）",
    params: {
      table: "{{table.reports}}",
      match_on: "week_key",
      data: {
        week_key: lw("/start"),
        week_start: lw("/start"),
        comment: body("/comment"),
        ...(withFile ? { report_file: { $from: "nodes.save_file.output", path: "/file_id" } } : {}),
      },
    },
  });
  return {
    ir_version: 1,
    name: "placeholder",
    display_name: opts.name,
    description: "先週の受注を地域ごとに集計して週次 KPI の表に書き、AI のコメントを添えた報告を週次報告のフォルダーに置く",
    declared_scopes: ["data.read", "data.write", "storage.write"],
    triggers: [{ kind: "schedule", cron: "0 7 * * 1", tz: "Asia/Tokyo", catchup: "skip" }],
    nodes: [
      { id: "last_week", type: "transform.date", label: "先週", params: { shift: { amount: -1, unit: "week" }, period: "week" } },
      { id: "prev_week", type: "transform.date", label: "前々週", params: { shift: { amount: -2, unit: "week" }, period: "week" } },
      wonQuery("won", "地域ごとの受注", lw),
      {
        id: "closed",
        type: "data.query",
        label: "地域ごとの決着",
        params: {
          table: "{{table.deals}}",
          query: {
            filter: {
              all: [
                { cmp: { field: "stage", op: "in", value: ["受注", "失注"] } },
                { cmp: { field: "close_date", op: "gte", value: lw("/start") } },
                { cmp: { field: "close_date", op: "lte", value: lw("/end") } },
              ],
            },
            group_by: ["region"],
            aggregate: [{ metric: "count" }],
            group_limit: 50,
          },
        },
      },
      wonQuery("won_prev", "前週の受注", pw),
      {
        id: "compute",
        type: "script.run",
        label: "KPI の行と表",
        params: {
          source: { inline: COMPUTE_SCRIPT },
          input: {
            start: lw("/start"),
            won: { $from: "nodes.won.output", path: "/groups" },
            closed: { $from: "nodes.closed.output", path: "/groups" },
            prev: { $from: "nodes.won_prev.output", path: "/groups" },
          },
        },
      },
      {
        id: "any",
        type: "control.branch",
        label: "書く行があるか",
        params: { condition: { cmp: { left: compute("/count"), op: "gt", right: 0 } } },
      },
      {
        id: "save",
        type: "data.bulk_upsert",
        label: "週次 KPI に書く",
        params: { table: "{{table.kpi}}", rows: compute("/rows"), match_on: "snapshot_key", abort_on_invalid: true },
      },
      { id: "saved", type: "control.join", label: "KPI を書いた後", params: { mode: "any" } },
      comment,
      { id: "j", type: "control.join", label: "コメントの後", params: { mode: "any" } },
      {
        id: "body",
        type: "script.run",
        label: "報告の本文",
        params: {
          source: { inline: BODY_SCRIPT },
          input: {
            start: lw("/start"),
            end: lw("/end"),
            table_md: compute("/table_md"),
            text: { $from: "nodes.comment.output", path: "/text", default: "" },
            stop: { $from: "nodes.comment.output", path: "/stop_reason", default: "" },
            err: { $from: "nodes.comment.output", path: "/error/code", default: "" },
          },
        },
      },
      {
        id: "save_file",
        type: "storage.write",
        label: "報告を保存",
        on_error: "continue",
        params: {
          folder: "{{folder.weekly}}",
          name: { $template: "KPI-{start}.md", vars: { start: lw("/start") } },
          content: body("/md"),
          content_type: "text/markdown",
          on_name_taken: "fail",
        },
      },
      report("save_report", true),
      report("save_report_nofile", false),
    ],
    edges: [
      { from: "last_week", to: "prev_week" },
      { from: "prev_week", to: "won" },
      { from: "won", to: "closed" },
      { from: "closed", to: "won_prev" },
      { from: "won_prev", to: "compute" },
      { from: "compute", to: "any" },
      { from: "any", from_port: "true", to: "save" },
      { from: "save", to: "saved" },
      { from: "any", from_port: "false", to: "saved" },
      { from: "saved", to: "comment" },
      { from: "comment", to: "j" },
      { from: "comment", from_port: "error", to: "j" },
      { from: "j", to: "body" },
      { from: "body", to: "save_file" },
      { from: "save_file", to: "save_report" },
      { from: "save_file", from_port: "error", to: "save_report_nofile" },
    ],
  };
}

/// 最新の週の script（最新の週の行だけで表を組み、伏せた地域は注記にする）。グラフ（```ui bar）は返さない:
/// workflow.reply の Markdown の図のフェンスは、操作の無い図も含めて全部 ```text に書き換えられ、図にならずに
/// 生の記述が出る（crates/yui の neutralize_fences・findings を参照）。作る人は表で返す。
const LATEST_SCRIPT = `function main(i) {
  var items = i.items || [];
  if (!items.length) return { md: "まだ週次 KPI がありません。月曜 7 時の集計の後に、もう一度押してください。" };
  var week = items[0].data.week_start;
  var rows = items.filter(function (r) { return r.data.week_start === week; });
  rows.sort(function (a, b) { return a.data.region < b.data.region ? -1 : 1; });
  var shown = rows.filter(function (r) { return r.data.won_amount !== null && r.data.won_amount !== undefined; });
  var hidden = rows.filter(function (r) { return r.data.won_amount === null || r.data.won_amount === undefined; });
  var md = "**" + week + " の週の地域別の受注額**\\n\\n";
  if (shown.length) {
    md += "| 地域 | 受注額 | 前週比 |\\n| --- | ---: | ---: |\\n";
    md += shown.map(function (r) {
      var wow = r.data.wow_pct === null || r.data.wow_pct === undefined ? "—" : String(r.data.wow_pct) + "%";
      return "| " + r.data.region + " | " + yen(r.data.won_amount) + " | " + wow + " |";
    }).join("\\n") + "\\n";
  } else {
    md += "この週は、表示できる地域がありません。\\n";
  }
  if (hidden.length) {
    md += "\\n" + hidden.map(function (r) { return r.data.region; }).join("、") + " は件数が少ないため非表示です。\\n";
  }
  return { md: md };
}
function yen(x) { return String(Math.round(Number(x))).replace(/\\B(?=(\\d{3})+(?!\\d))/g, ",") + " 円"; }`;

/// 最新の週を見る（ボタン）。
const latestIr: Json = {
  ir_version: 1,
  name: "placeholder",
  display_name: "最新の週を見る",
  description: "週次 KPI の最新の週の、地域別の受注額と前週比を見る",
  declared_scopes: ["data.read"],
  triggers: [{ kind: "interactive" }],
  nodes: [
    {
      id: "latest",
      type: "data.query",
      label: "最新の週次 KPI",
      params: { table: "{{table.kpi}}", query: { sort: [{ field: "week_start", descending: true }], limit: 50 } },
    },
    {
      id: "table",
      type: "script.run",
      label: "表を組む",
      params: { source: { inline: LATEST_SCRIPT }, input: { items: { $from: "nodes.latest.output", path: "/items" } } },
    },
    {
      id: "show",
      type: "workflow.reply",
      label: "表を返す",
      params: { format: "markdown", tone: "neutral", value: { $from: "nodes.table.output", path: "/md" } },
    },
  ],
  edges: [
    { from: "latest", to: "table" },
    { from: "table", to: "show" },
  ],
};

const SCREEN = `毎週月曜 7 時に、先週の受注を地域ごとに集計します。件数が少ない地域は、個人が分からないよう数を伏せます。

## 週次 KPI

\`\`\`data_grid
週次 KPI
\`\`\`

\`\`\`ui
app 最新の週
button 最新の週を見る -> workflow 最新の週を見る
\`\`\`
`;

const AGG_SCREEN = `地域ごとの受注額の合計（集計のビュー）を表に出そうとした画面です。

\`\`\`data_grid
地域別の受注額
\`\`\`
`;

/// 有効化の委譲（同意計画が求めるとおり: 読むのは商談、書くのは週次 KPI と週次報告と報告のフォルダー）。
export const WEEKLY_GRANTS: GrantDef[] = [
  { scope: "data.read", kind: "data_table", target: "{{table.deals}}", relation: "viewer" },
  { scope: "data.write", kind: "data_table", target: "{{table.kpi}}", relation: "editor" },
  { scope: "data.write", kind: "data_table", target: "{{table.reports}}", relation: "editor" },
  { scope: "storage.write", kind: "folder", target: "{{folder.weekly}}", relation: "editor" },
];

/// コメントの LLM を失敗させる写し（ref → 写しの作り方・確かめる code）。
export const LLM_VARIANTS = {
  weekly_slow: { opts: { prefix: "slow:30 ", timeoutSec: 5 }, code: "step_timeout" },
  weekly_refuse: { opts: { prefix: "refuse:\n", shaped: true }, code: "llm_refused" },
  // 形の指定の無い呼び出しの送り直されない誤り（stub の BadRequest）は invalid になる（APP-03 と同じ）。
  weekly_fail: { opts: { prefix: "fail:コメントの確かめ\n" }, code: "invalid" },
} as const;

const KPI_FIELDS = [
  { key: "snapshot_key", display_name: "集計のキー", type: "text", unique: true, indexed: true },
  { key: "week_start", display_name: "週の初め", type: "date", indexed: true },
  { key: "region", display_name: "地域", type: "text", indexed: true },
  { key: "won_amount", display_name: "受注額", type: "number" },
  { key: "won_count", display_name: "受注件数", type: "number" },
  { key: "closed_count", display_name: "決着件数", type: "number" },
  { key: "win_rate", display_name: "受注率（%）", type: "number" },
  { key: "wow_pct", display_name: "前週比（%）", type: "number" },
  { key: "status", display_name: "状態", type: "text" },
];

/// APP-08 の世界の定義（本物の cron が数える週の商談は spec が置く・[`cronDealRows`]）。
export function app08World(): WorldDef {
  return {
    key: "app08",
    ai: { chat: true },
    roles: [{ ref: "keiei", members: ["alice"] }],
    folders: [{ ref: "weekly", name: "週次報告" }],
    tables: [
      {
        ref: "deals",
        name: "商談",
        schema: {
          fields: [
            { key: "name", display_name: "商談名", type: "text", indexed: true },
            { key: "region", display_name: "地域", type: "select", options: ["東", "西", "北", "南"], indexed: true },
            { key: "stage", display_name: "段階", type: "select", options: ["リード", "提案", "交渉", "受注", "失注"], indexed: true },
            { key: "amount", display_name: "金額", type: "number", numeric_spec: { scale: 0, min: 0 }, indexed: true },
            { key: "close_date", display_name: "決着日", type: "date", indexed: true },
            { key: "sales", display_name: "担当", type: "user_ref", indexed: true },
          ],
          row_policy: { read: { any: [OWN, KEIEI] }, write: { any: [OWN, KEIEI] } },
        },
        rows: DEALS.map(dealRow),
        shares: [{ user: "bob", role: "viewer" }],
      },
      {
        ref: "kpi",
        name: "週次 KPI",
        schema: { fields: KPI_FIELDS },
        shares: [{ user: "bob", role: "viewer" }],
      },
      {
        ref: "reports",
        name: "週次報告",
        schema: {
          fields: [
            { key: "week_key", display_name: "週のキー", type: "text", unique: true, indexed: true },
            { key: "week_start", display_name: "週の初め", type: "date", indexed: true },
            { key: "comment", display_name: "コメント（AI）", type: "text" },
            { key: "report_file", display_name: "報告のファイル", type: "file_ref" },
          ],
        },
      },
    ],
    views: [
      {
        ref: "kpi",
        name: "週次 KPI",
        table: "kpi",
        query: { sort: [{ field: "week_start", descending: true }, { field: "region" }], limit: 100 },
        // 週・地域・状態を先に並べ、集計のキーは隠す（スマホ幅でも伏せた地域が分かるように）。
        display: {
          kind: "grid",
          columns: [
            { field: "week_start", frozen: true, width: 110 },
            { field: "region", frozen: true, width: 64 },
            { field: "status", width: 190 },
            { field: "won_amount", width: 120 },
            { field: "won_count", width: 90 },
            { field: "closed_count", width: 90 },
            { field: "win_rate", width: 110 },
            { field: "wow_pct", width: 110 },
            { field: "snapshot_key", hidden: true },
          ],
        },
      },
      {
        ref: "agg",
        name: "地域別の受注額",
        table: "deals",
        query: {
          filter: { cmp: { field: "stage", op: "eq", value: "受注" } },
          group_by: ["region"],
          aggregate: [{ metric: "sum", field: "amount" }],
        },
      },
    ],
    workflows: [
      { ref: "weekly", ir: weeklyIr({ name: "週次 KPI" }), enable: { grants: WEEKLY_GRANTS } },
      ...Object.entries(LLM_VARIANTS).map(([ref, v]) => ({ ref, ir: weeklyIr({ name: `週次 KPI（${ref}）`, ...v.opts }) })),
      { ref: "latest", ir: latestIr },
    ],
    apps: [
      {
        ref: "main",
        name: "週次 KPI",
        // アプリの画面は説明（body.description）を名前として見出しに出すので短くする（APP-03 と同じ）。
        description: "週次 KPI",
        screen: SCREEN,
        workflows: [{ alias: "最新の週を見る", wf: "latest" }],
        views: [{ alias: "週次 KPI", view: "kpi" }],
        shares: [{ user: "bob", role: "viewer" }],
      },
      {
        ref: "agg",
        name: "受注の集計（ビュー）",
        description: "受注の集計",
        screen: AGG_SCREEN,
        workflows: [],
        views: [{ alias: "地域別の受注額", view: "agg" }],
        shares: [{ user: "bob", role: "viewer" }],
      },
    ],
  };
}
