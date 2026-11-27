import type { Json } from "../http";
import type { WorldDef } from "../world";

/// APP-06 在庫の発注点アラートと発注書の下書き（小売・製造・台帳 §7・§7.13）の世界。
///
/// - 人: alice は倉庫（spec ごとの部署 `{{role.wh}}`）で、表・ワークフロー・アプリの持ち主・発注点チェックの有効化者
///   （委譲元）。bob は購買（`{{role.buy}}`）。2 人とも入出庫のボタンを押す（対話の run は押した本人の権限で書くので、
///   bob を品目と発注の表の editor にする・状態以外の列も直接書き換えられるのは台帳の blocker）。
/// - 表: 品目（SKU は重複しない・在庫は 0 以上・不足 = 発注点 − 在庫 の索引つきの数式）と発注（発注のキー
///   `SKU:日付` は重複しない・状態の進め方つき: 下書き → 発注済 → 入荷済、下書き → 取消）。仕入先は品目の文字の列に
///   置く（record_ref は世界の定義から行の id を指せないため・判定に関わらない）。
/// - ワークフロー:
///   - 入出庫（ボタン）: SKU で品目を探し、出庫で在庫が足りるかを script で確かめ、「足す数」（`add`）で在庫を
///     書き換える。同時に出庫して在庫が足りなくなった側は、列の最小値（0）で断られ（`invalid`・GAP-117）、error の
///     色の理由を返す。発注点を下回ったら warning の色で知らせる（購買への通知は GAP-06 で見送り・表と reply で代える）。
///     台帳 §7 の「branch → workflow.start」で発注点チェックをすぐ起こす形は置かない: 子の run は押した人の権限で動き、
///     結果を誰にも見せない（ir.md §7.10）ので、倉庫の人に発注の表とフォルダーの書き込みを渡すことになる。発注は
///     毎朝のスケジュールに任せる。
///   - 入出庫（待ち）: 同時に出庫したときの rev の競合を決まって通す試験用の版（§7.13 共通の決まり「同時操作」）。
///     探した直後に「読んだときの在庫と版」を結果に出し（押した本人の記録で読める・他人の run を /workflows から
///     読まない #917）、[`HOLD_SEC`] 秒待ってから、新しい在庫を script で計算して `expected_rev` つきで書く。2 つの
///     run が両方とも同じ版を読んでから書くので、負けた側は `conflict` の節を決まって通る（spec は 2 人が同じ版を
///     読んだことを確かめる・APP-11 の貸出（待ち）と同じ形）。
///   - 発注点チェック（毎朝 8 時・Asia/Tokyo）: 今日の日付 → 不足 > 0 の品目 → 未完了（下書き・発注済）の発注 →
///     script（未完了の発注のある SKU を除き、数量 = ceil(不足 / ロット) × ロット・発注のキー = SKU:日付）→
///     data.bulk_upsert（match_on 発注のキー・同じ日に重ねて流しても 2 行にならない）→ 作った件数（適用 − 一致）が
///     1 以上のときだけ csv.write（発注書-日付.csv・列は宣言の順）。
/// 架空の名前だけを使う（public リポジトリ）。

/// 品目（SKU → 初めの値）。不足は 発注点 − 在庫（発注点の無い品目は不足も無い）。
export const ITEM = {
  /// 不足 7・ロット 12 → 12 個。(2)(c)(d) で発注済 → 入荷済 に進める。
  s101: { sku: "S-101", name: "六角ボルト M8", stock: 3, reorder_point: 10, lot_size: 12, supplier: "ことり精工", lead_time_days: 5 },
  /// 不足 20・ロット 10 → 20 個（ちょうどロットの倍数）。
  s102: { sku: "S-102", name: "六角ナット M8", stock: 0, reorder_point: 20, lot_size: 10, supplier: "ことり精工", lead_time_days: 5 },
  /// 在庫 = 発注点（不足 0）→ 発注しない。
  s103: { sku: "S-103", name: "平ワッシャー M8", stock: 8, reorder_point: 8, lot_size: 50, supplier: "ことり精工", lead_time_days: 5 },
  /// 在庫 > 発注点 → 発注しない。
  s104: { sku: "S-104", name: "ばね座金 M8", stock: 30, reorder_point: 10, lot_size: 50, supplier: "ことり精工", lead_time_days: 5 },
  /// 発注点なし → 発注しない。
  s105: { sku: "S-105", name: "結束バンド 200mm", stock: 1, lot_size: 100, supplier: "ほしぞら資材", lead_time_days: 3 },
  /// 不足 4 だが、発注済の発注がある → 発注しない。
  s106: { sku: "S-106", name: "配線ケーブル 2m", stock: 2, reorder_point: 6, lot_size: 5, supplier: "ほしぞら資材", lead_time_days: 3 },
  /// 不足 4・ロット 4 → 4 個（前の発注は取消なので未完了でない）。
  s107: { sku: "S-107", name: "端子台 6P", stock: 1, reorder_point: 5, lot_size: 4, supplier: "ほしぞら資材", lead_time_days: 7 },
  /// 初めは足りている。bob が画面から 5 個出庫すると 7（不足 3・ロット 6 → 6 個）。
  u201: { sku: "U-201", name: "保護メガネ", stock: 12, reorder_point: 10, lot_size: 6, supplier: "ほしぞら資材", lead_time_days: 2 },
  /// 初めは足りている。alice が 4 個出庫すると 7（不足 3・ロット 10 → 10 個）。
  u202: { sku: "U-202", name: "作業手袋 L", stock: 11, reorder_point: 10, lot_size: 10, supplier: "ほしぞら資材", lead_time_days: 2 },
  /// 本番の IR で alice と bob が同時に 3 個ずつ出庫する（在庫 5）。
  r301: { sku: "R-301", name: "安全靴 26cm", stock: 5, reorder_point: 1, lot_size: 1, supplier: "ことり精工", lead_time_days: 10 },
  /// 待ちを挟む版で同時に 3 個ずつ出庫する（在庫 5）。
  r302: { sku: "R-302", name: "ヘルメット 白", stock: 5, reorder_point: 1, lot_size: 1, supplier: "ことり精工", lead_time_days: 10 },
} as const;

/// 待ちを挟む版の待ち（秒）。2 つの run の「探す」の差がこれより短ければ、両方が同じ版を読んでから書く（ワーカーは
/// 0.2 秒ごとに run を拾う・遅い機械と next dev の初回の応答の遅れを見込んで長めにする・APP-11 の HOLD_SEC と同じ）。
export const HOLD_SEC = 15;

/// 待ちを挟む版の、探した直後の結果の文。
export const seenText = (stock: number | string, rev: number | string) => `読んだときの在庫: ${stock}（版 ${rev}）`;

/// 登録の無い SKU。
export const MISSING_SKU = "X-999";

/// 前からある発注（beforeAll で bob が S-106 を発注済、S-107 を取消に進める）。
export const OLD_PO = {
  s106: { po_key: "S-106:2026-11-01", po_no: "PO-20261101-S-106", sku: "S-106", item_name: "配線ケーブル 2m", qty: 5, supplier: "ほしぞら資材", ordered_on: "2026-11-01", due_on: "2026-11-04" },
  s107: { po_key: "S-107:2026-11-01", po_no: "PO-20261101-S-107", sku: "S-107", item_name: "端子台 6P", qty: 4, supplier: "ほしぞら資材", ordered_on: "2026-11-01", due_on: "2026-11-08" },
} as const;

/// 発注書.csv の列（宣言の順）。
export const CSV_COLUMNS = ["発注番号", "SKU", "品名", "数量", "仕入先", "納期"];

const find = {
  id: "find",
  type: "data.query",
  label: "品目を探す",
  params: {
    table: "{{table.items}}",
    query: { filter: { cmp: { field: "sku", op: "eq", value: { $from: "input", path: "/sku" } } }, limit: 1 },
  },
};

const has = {
  id: "has",
  type: "control.branch",
  label: "登録があるか",
  params: {
    condition: { cmp: { left: { $from: "nodes.find.output", path: "/items/0/id", default: "" }, op: "neq", right: "" } },
  },
};

const reply = (id: string, label: string, tone: string, value: unknown) => ({
  id,
  type: "workflow.reply",
  label,
  params: { format: "text", tone, value },
});

const item = (path: string) => ({ $from: "nodes.find.output", path: `/items/0${path}` });
const sku = { $from: "input", path: "/sku" };

const missing = reply("missing", "登録が無い", "error", {
  $template: "SKU「{sku}」の品目は登録がありません。SKU を確かめてください",
  vars: { sku },
});

const short = reply("short", "在庫が足りない", "error", {
  $template: "在庫が足りません。{name}（{sku}）のいまの在庫は {stock} です",
  vars: { name: item("/data/name"), sku, stock: item("/data/stock") },
});

/// 入出庫の増減と、出庫で在庫が足りるか（足りないときは書かずに理由を返す）。
const CALC_SCRIPT = `function main(input) {
  var stock = Number(input.stock) || 0;
  var qty = Number(input.qty) || 0;
  var delta = input.kind === "out" ? -qty : qty;
  return { delta: delta, next: stock + delta, enough: stock + delta >= 0 };
}`;

const calc = {
  id: "calc",
  type: "script.run",
  label: "増減を計算",
  params: {
    source: { inline: CALC_SCRIPT },
    input: { stock: item("/data/stock"), qty: { $from: "input", path: "/qty" }, kind: { $from: "input", path: "/kind" } },
  },
};

const enough = {
  id: "enough",
  type: "control.branch",
  label: "在庫が足りるか",
  params: { condition: { cmp: { left: { $from: "nodes.calc.output", path: "/enough" }, op: "eq", right: true } } },
};

const INPUT_SCHEMA: Json = {
  type: "object",
  properties: {
    sku: { type: "string", title: "SKU" },
    qty: { type: "integer", minimum: 1, maximum: 100000, title: "数量" },
    kind: { type: "string", enum: ["in", "out"], title: "区分" },
  },
  required: ["sku", "qty", "kind"],
  "x-order": ["sku", "qty", "kind"],
};

/// 入出庫（本番の IR）: 足す数で在庫を書き換える（間にほかの書き込みがあれば読み直して足し直す・GAP-16）。
const moveIr: Json = {
  ir_version: 1,
  name: "placeholder",
  display_name: "入出庫",
  description: "SKU の品目の在庫を、入庫なら増やし、出庫なら減らす。発注点を下回ったら知らせる",
  declared_scopes: ["data.read", "data.write"],
  triggers: [{ kind: "interactive" }],
  input_schema: INPUT_SCHEMA,
  nodes: [
    find,
    has,
    missing,
    calc,
    enough,
    short,
    {
      id: "move",
      type: "data.record.update",
      label: "在庫を書き換える",
      on_error: "continue",
      params: { table: "{{table.items}}", record: item("/id"), add: { stock: { $from: "nodes.calc.output", path: "/delta" } } },
    },
    {
      id: "low",
      type: "control.branch",
      label: "発注点を下回ったか",
      params: { condition: { cmp: { left: { $from: "nodes.move.output", path: "/data/shortage", default: 0 }, op: "gt", right: 0 } } },
    },
    reply("alert", "発注点を下回った", "warning", {
      $template: "{name}（{sku}）の在庫は {stock} になりました。発注点の {rp} を下回ったので、明朝の発注点チェックで発注書の下書きを作ります",
      vars: {
        name: { $from: "nodes.move.output", path: "/data/name" },
        sku,
        stock: { $from: "nodes.move.output", path: "/data/stock" },
        rp: { $from: "nodes.move.output", path: "/data/reorder_point" },
      },
    }),
    reply("done", "登録した", "success", {
      $template: "{name}（{sku}）の在庫は {stock} になりました",
      vars: { name: { $from: "nodes.move.output", path: "/data/name" }, sku, stock: { $from: "nodes.move.output", path: "/data/stock" } },
    }),
    {
      id: "why",
      type: "control.switch",
      label: "書き換えられなかった理由",
      params: { value: { $from: "nodes.move.output", path: "/error/code" }, cases: [{ port: "raced", equals: "invalid" }] },
    },
    reply("raced", "重なって足りなくなった", "error", "ほかの人の登録と重なって、在庫が足りなくなりました。表で在庫を確かめてから、もう一度登録してください"),
    reply("failed", "登録できなかった", "error", "登録できませんでした。もう一度押すか、倉庫の担当に連絡してください"),
  ],
  edges: [
    { from: "find", to: "has" },
    { from: "has", from_port: "false", to: "missing" },
    { from: "has", from_port: "true", to: "calc" },
    { from: "calc", to: "enough" },
    { from: "enough", from_port: "false", to: "short" },
    { from: "enough", from_port: "true", to: "move" },
    { from: "move", to: "low" },
    { from: "low", from_port: "true", to: "alert" },
    { from: "low", from_port: "false", to: "done" },
    { from: "move", from_port: "error", to: "why" },
    { from: "why", from_port: "raced", to: "raced" },
    { from: "why", from_port: "default", to: "failed" },
  ],
};

/// 入出庫（待ちを挟む試験用の版）: 探してから `hold` 秒待ち、読んだ rev を前提に新しい在庫を書く。
function moveWaitIr(hold: number): Json {
  return {
    ir_version: 1,
    name: "placeholder",
    display_name: "入出庫（待ち）",
    description: "同時に登録したときの rev の競合を確かめる版（探してから書くまでに待ちを挟み、読んだ版を前提に書く）",
    declared_scopes: ["data.read", "data.write"],
    triggers: [{ kind: "interactive" }],
    input_schema: INPUT_SCHEMA,
    nodes: [
      find,
      has,
      missing,
      reply("seen", "読んだ在庫", "neutral", {
        $template: "読んだときの在庫: {stock}（版 {rev}）",
        vars: { stock: item("/data/stock"), rev: item("/rev") },
      }),
      { id: "hold", type: "control.wait", label: "少し待つ", params: { kind: "duration", duration_sec: hold } },
      calc,
      enough,
      short,
      {
        id: "move",
        type: "data.record.update",
        label: "在庫を書き換える",
        on_error: "continue",
        params: {
          table: "{{table.items}}",
          record: item("/id"),
          patch: { stock: { $from: "nodes.calc.output", path: "/next" } },
          expected_rev: item("/rev"),
        },
      },
      reply("done", "登録した", "success", {
        $template: "{name}（{sku}）の在庫は {stock} になりました",
        vars: { name: { $from: "nodes.move.output", path: "/data/name" }, sku, stock: { $from: "nodes.move.output", path: "/data/stock" } },
      }),
      {
        id: "why",
        type: "control.switch",
        label: "書き換えられなかった理由",
        params: { value: { $from: "nodes.move.output", path: "/error/code" }, cases: [{ port: "conflict", equals: "conflict" }] },
      },
      reply("clash", "同時に登録された", "error", "ほかの人が同時に登録しました。表を読み直して、もう一度登録してください"),
      reply("failed", "登録できなかった", "error", "登録できませんでした。もう一度押すか、倉庫の担当に連絡してください"),
    ],
    edges: [
      { from: "find", to: "has" },
      { from: "has", from_port: "false", to: "missing" },
      { from: "has", from_port: "true", to: "seen" },
      { from: "seen", to: "hold" },
      { from: "hold", to: "calc" },
      { from: "calc", to: "enough" },
      { from: "enough", from_port: "false", to: "short" },
      { from: "enough", from_port: "true", to: "move" },
      { from: "move", to: "done" },
      { from: "move", from_port: "error", to: "why" },
      { from: "why", from_port: "conflict", to: "clash" },
      { from: "why", from_port: "default", to: "failed" },
    ],
  };
}

/// 発注の下書きと発注書の行を組む script（未完了の発注のある SKU を除き、ロットで切り上げる）。日付は入力で受ける
/// （script の時計は実時刻を返さない・BUG-283）。
const PLAN_SCRIPT = `function main(input) {
  var open = {};
  (input.open || []).forEach(function (r) { open[r.data.sku] = true; });
  var day = input.date;
  var rows = [];
  var lines = [];
  (input.items || []).forEach(function (r) {
    var d = r.data;
    var short = Number(d.shortage);
    if (open[d.sku] || !(short > 0)) return;
    var lot = Number(d.lot_size) > 0 ? Number(d.lot_size) : 1;
    var qty = Math.ceil(short / lot) * lot;
    var due = addDays(day, Number(d.lead_time_days) || 0);
    var key = d.sku + ":" + day;
    var no = "PO-" + day.replace(/-/g, "") + "-" + d.sku;
    rows.push({ key: key, data: { po_key: key, po_no: no, sku: d.sku, item_name: d.name, qty: qty, supplier: d.supplier || "", ordered_on: day, due_on: due } });
    lines.push([no, d.sku, d.name, String(qty), d.supplier || "", due]);
  });
  return { rows: rows, count: rows.length, csv: { columns: ${JSON.stringify(CSV_COLUMNS)}, rows: lines } };
}
function addDays(day, n) {
  var p = day.split("-");
  return new Date(Date.UTC(Number(p[0]), Number(p[1]) - 1, Number(p[2]) + n)).toISOString().slice(0, 10);
}`;

const CREATED_SCRIPT = `function main(input) {
  return { created: (Number(input.applied) || 0) - (Number(input.matched) || 0) };
}`;

/// 発注点チェック（毎朝 8 時）。
const checkIr: Json = {
  ir_version: 1,
  name: "placeholder",
  display_name: "発注点チェック",
  description: "発注点を下回り、まだ発注していない品目の発注の下書きを作り、発注書の CSV を発注書のフォルダーに置く",
  declared_scopes: ["data.read", "data.write", "csv.write"],
  triggers: [{ kind: "schedule", cron: "0 8 * * *", tz: "Asia/Tokyo", catchup: "skip" }],
  nodes: [
    { id: "day", type: "transform.date", label: "今日", params: {} },
    {
      id: "short",
      type: "data.query",
      label: "発注点を下回った品目",
      params: {
        table: "{{table.items}}",
        query: { filter: { cmp: { field: "shortage", op: "gt", value: 0 } }, sort: [{ field: "sku" }], limit: 200 },
      },
    },
    {
      id: "open",
      type: "data.query",
      label: "未完了の発注",
      params: {
        table: "{{table.orders}}",
        query: { filter: { cmp: { field: "status", op: "in", value: ["下書き", "発注済"] } }, limit: 200 },
      },
    },
    {
      id: "plan",
      type: "script.run",
      label: "発注の下書きを組む",
      params: {
        source: { inline: PLAN_SCRIPT },
        input: {
          items: { $from: "nodes.short.output", path: "/items" },
          open: { $from: "nodes.open.output", path: "/items" },
          date: { $from: "nodes.day.output", path: "/date" },
        },
      },
    },
    {
      id: "any",
      type: "control.branch",
      label: "発注する品目があるか",
      params: { condition: { cmp: { left: { $from: "nodes.plan.output", path: "/count" }, op: "gt", right: 0 } } },
    },
    {
      id: "save",
      type: "data.bulk_upsert",
      label: "発注の下書きを登録",
      params: { table: "{{table.orders}}", rows: { $from: "nodes.plan.output", path: "/rows" }, match_on: "po_key" },
    },
    {
      id: "made",
      type: "script.run",
      label: "新しく作った件数",
      params: {
        source: { inline: CREATED_SCRIPT },
        input: {
          applied: { $from: "nodes.save.output", path: "/rows_applied" },
          matched: { $from: "nodes.save.output", path: "/rows_matched", default: 0 },
        },
      },
    },
    {
      id: "fresh",
      type: "control.branch",
      label: "新しく作ったか",
      params: { condition: { cmp: { left: { $from: "nodes.made.output", path: "/created" }, op: "gt", right: 0 } } },
    },
    {
      id: "sheet",
      type: "csv.write",
      label: "発注書を保存",
      params: {
        folder: "{{folder.po}}",
        name: { $template: "発注書-{date}", vars: { date: { $from: "nodes.day.output", path: "/date" } } },
        content: { $from: "nodes.plan.output", path: "/csv" },
        on_name_taken: "add_number",
        encoding: "utf8_bom",
      },
    },
  ],
  edges: [
    { from: "day", to: "short" },
    { from: "short", to: "open" },
    { from: "open", to: "plan" },
    { from: "plan", to: "any" },
    { from: "any", from_port: "true", to: "save" },
    { from: "save", to: "made" },
    { from: "made", to: "fresh" },
    { from: "fresh", from_port: "true", to: "sheet" },
  ],
};

const SCREEN = `SKU は品目のラベルに書いてあります。出庫で発注点を下回った品目は、毎朝 8 時の発注点チェックで発注書の下書きになります。

\`\`\`ui
app 入出庫の登録
SKU = text 必須 as sku
数量 = number 1..100000 必須 as qty
区分 = choice 入庫=in | 出庫=out 必須 as kind
button 登録 -> workflow 入出庫
\`\`\`

## 発注点を下回った品目

\`\`\`data_grid
発注点割れの品目
\`\`\`

## 下書きの発注

\`\`\`data_grid
下書きの発注
\`\`\`
`;

const WAIT_SCREEN = `同時に出庫したときの確かめに使う版です。

\`\`\`ui
app 入出庫の登録（待ちを挟む版）
SKU = text 必須 as sku
数量 = number 1..100000 必須 as qty
区分 = choice 入庫=in | 出庫=out 必須 as kind
button 登録 -> workflow 入出庫（待ち）
\`\`\`
`;

const ROLE_BUY = { has_role: { role: "{{role.buy}}", subtree: true } };
const ROLE_WH = { has_role: { role: "{{role.wh}}", subtree: true } };

const int0 = { scale: 0, min: 0 };

/// APP-06 の世界の定義。
export function app06World(): WorldDef {
  return {
    key: "app06",
    roles: [
      { ref: "wh", members: ["alice"] },
      { ref: "buy", members: ["bob"] },
    ],
    folders: [{ ref: "po", name: "発注書", shares: [{ user: "bob", role: "viewer" }] }],
    tables: [
      {
        ref: "items",
        name: "品目",
        schema: {
          fields: [
            { key: "sku", display_name: "SKU", type: "text", unique: true, indexed: true },
            { key: "name", display_name: "品名", type: "text" },
            { key: "stock", display_name: "在庫", type: "number", numeric_spec: int0, indexed: true },
            { key: "reorder_point", display_name: "発注点", type: "number", numeric_spec: int0 },
            { key: "lot_size", display_name: "発注の単位", type: "number", numeric_spec: { scale: 0, min: 1 } },
            { key: "supplier", display_name: "仕入先", type: "text" },
            { key: "lead_time_days", display_name: "納期の日数", type: "number", numeric_spec: int0 },
            {
              key: "shortage",
              display_name: "不足",
              type: "formula",
              formula: { sub: [{ field: "reorder_point" }, { field: "stock" }] },
              indexed: true,
            },
          ],
        },
        rows: Object.values(ITEM).map((x) => ({ ...x })),
        shares: [{ user: "bob", role: "editor" }],
      },
      {
        ref: "orders",
        name: "発注",
        schema: {
          fields: [
            { key: "po_key", display_name: "発注のキー", type: "text", unique: true, indexed: true },
            { key: "po_no", display_name: "発注番号", type: "text" },
            { key: "sku", display_name: "SKU", type: "text", indexed: true },
            { key: "item_name", display_name: "品名", type: "text" },
            { key: "qty", display_name: "数量", type: "number", numeric_spec: { scale: 0, min: 1 } },
            { key: "supplier", display_name: "仕入先", type: "text" },
            { key: "ordered_on", display_name: "発注日", type: "date", indexed: true },
            { key: "due_on", display_name: "納期", type: "date" },
            { key: "status", display_name: "状態", type: "select", options: ["下書き", "発注済", "入荷済", "取消"], indexed: true },
          ],
          status_field: "status",
        },
        fsm: {
          status_field: "status",
          states: ["下書き", "発注済", "入荷済", "取消"],
          transitions: [
            { from: "下書き", to: "発注済", actor: ROLE_BUY },
            { from: "下書き", to: "取消", actor: ROLE_BUY },
            { from: "発注済", to: "入荷済", actor: ROLE_WH },
          ],
        },
        rows: Object.values(OLD_PO).map((x) => ({ ...x })),
        shares: [{ user: "bob", role: "editor" }],
      },
    ],
    views: [
      {
        ref: "shortage",
        name: "発注点割れの品目",
        table: "items",
        query: { filter: { cmp: { field: "shortage", op: "gt", value: 0 } }, sort: [{ field: "sku" }] },
      },
      {
        ref: "drafts",
        name: "下書きの発注",
        table: "orders",
        query: { filter: { cmp: { field: "status", op: "eq", value: "下書き" } }, sort: [{ field: "sku" }] },
      },
    ],
    workflows: [
      { ref: "move", ir: moveIr },
      { ref: "move_wait", ir: moveWaitIr(HOLD_SEC) },
      {
        ref: "check",
        ir: checkIr,
        enable: {
          grants: [
            { scope: "data.read", kind: "data_table", target: "{{table.items}}", relation: "viewer" },
            { scope: "data.read", kind: "data_table", target: "{{table.orders}}", relation: "viewer" },
            { scope: "data.write", kind: "data_table", target: "{{table.orders}}", relation: "editor" },
            { scope: "csv.write", kind: "folder", target: "{{folder.po}}", relation: "editor" },
          ],
        },
      },
    ],
    apps: [
      {
        ref: "main",
        name: "入出庫の登録",
        // アプリの画面は説明（body.description）を名前として見出しに出す（apps/[id]/page.tsx の appName）ので短くする。
        description: "入出庫の登録",
        screen: SCREEN,
        workflows: [{ alias: "入出庫", wf: "move" }],
        views: [
          { alias: "発注点割れの品目", view: "shortage" },
          { alias: "下書きの発注", view: "drafts" },
        ],
        shares: [{ user: "bob", role: "viewer" }],
      },
      {
        ref: "wait",
        name: "入出庫の登録（待ちを挟む版）",
        description: "入出庫の登録（待ちを挟む版）",
        screen: WAIT_SCREEN,
        workflows: [{ alias: "入出庫（待ち）", wf: "move_wait" }],
        shares: [{ user: "bob", role: "viewer" }],
      },
    ],
  };
}
