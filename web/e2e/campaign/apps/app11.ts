import type { Json } from "../http";
import type { WorldDef } from "../world";

/// APP-11 備品の貸出管理（総務・情シス・台帳 §7）の世界。
///
/// - 人: alice は総務（spec ごとの部署 `{{role.soumu}}`）で、表・ワークフロー・アプリの持ち主。同時に借りる確かめでは
///   借り手も兼ねる。bob は社員（借り手）。role_ref の列と部署への共有は使わない（ディレクトリが要る・GAP-76）。
/// - 表: 備品（資産番号は重複しない・状態の進め方つき）と貸出記録（本人か総務だけが読み書きできる）。対話の run は
///   押した本人の権限で書くので、bob を両方の表の editor にする（台帳の blocker: 状態以外の列も直接書き換えられる）。
/// - ワークフロー: 貸出・返却（ボタン）と延滞チェック（毎朝 9 時）。貸出は「読み直してから rev つきで遷移」で、
///   二重の貸出を遷移元の検査と rev で防ぐ（台帳 §16 の改訂版の形）。延滞チェックは期日切れで未返却の行の一覧を総務の
///   フォルダーに置き、まだ印の無い行にだけ印（overdue_on）を付ける（真偽の型が無いので日付で印を付ける・GAP-25）。
/// - 同時押しの確かめ用の版（貸出（待ち））は、探してから遷移までに 4 秒の待ちを挟み、2 つの run が両方とも
///   「在庫」を読んでから遷移するようにする（rev の競合の節を決まって通す）。本番の版には待ちを入れない。
/// 架空の名前だけを使う（public リポジトリ）。

/// 備品の資産番号（表は流しごとに作るので、番号は固定でよい）。
export const ASSET = {
  /// 本番の版で同時に借りる。
  race: "E-101",
  /// 待ちを挟む版で同時に借りる。
  raceWait: "E-102",
  /// 同じ人の二度押し・同じ冪等キーの送り直し。
  twice: "E-103",
  /// bob が画面から借りて返す（前に返し終わった貸出記録が 1 件ある）。
  bobUi: "E-104",
  /// alice が借りていて、bob が返そうとする。
  alices: "E-105",
  /// 登録の無い番号。
  missing: "E-999",
} as const;

/// 延滞の確かめの貸出記録（備品の表には無い番号・2026-11-09 9:00 の予定の日時で数える）。
export const OVERDUE_AT = "2026-11-09T09:00";
export const OVERDUE_DAY = "2026-11-09";
export const LOAN = {
  /// bob・期日 11-05・未返却 → 11-09 に延滞。
  bobLate: { asset_no: "E-901", borrower: "bob", out_date: "2026-10-29", due_date: "2026-11-05" },
  /// alice・期日 11-08・未返却 → 11-09 に延滞。
  aliceLate: { asset_no: "E-902", borrower: "alice", out_date: "2026-11-01", due_date: "2026-11-08" },
  /// bob・期日 11-09（当日）→ 11-09 には延滞でない。
  bobToday: { asset_no: "E-903", borrower: "bob", out_date: "2026-11-02", due_date: "2026-11-09" },
  /// alice・期日 11-12 → 11-09 には延滞でない。
  aliceLater: { asset_no: "E-904", borrower: "alice", out_date: "2026-11-05", due_date: "2026-11-12" },
  /// bob・期日 11-01・返却済み → 延滞でない。
  bobReturned: { asset_no: "E-905", borrower: "bob", out_date: "2026-10-25", due_date: "2026-11-01", returned_on: "2026-11-02" },
  /// E-104 の前の貸出（返却済み・返却で変わらない）。
  bobUiPast: { asset_no: "E-104", borrower: "alice", out_date: "2026-10-01", due_date: "2026-10-08", returned_on: "2026-10-05" },
} as const;

const ROLE = { has_role: { role: "{{role.soumu}}", subtree: true } };
const BORROWER = { field_cmp: { field: "borrower", op: "eq", value: "user_id" } };

const find = {
  id: "find",
  type: "data.query",
  label: "備品を探す",
  params: {
    table: "{{table.equipment}}",
    query: { filter: { cmp: { field: "asset_no", op: "eq", value: { $from: "input", path: "/asset_no" } } }, limit: 1 },
  },
};

const status = (cases: { port: string; equals: string }[]) => ({
  id: "state",
  type: "control.switch",
  label: "備品の状態",
  params: { value: { $from: "nodes.find.output", path: "/items/0/data/status", default: "なし" }, cases },
});

const reply = (id: string, label: string, tone: string, value: unknown) => ({
  id,
  type: "workflow.reply",
  label,
  params: { format: "text", tone, value },
});

const missing = reply("missing", "登録が無い", "error", {
  $template: "資産番号「{no}」の備品は登録がありません。番号を確かめてください",
  vars: { no: { $from: "input", path: "/asset_no" } },
});

const record = { $from: "nodes.find.output", path: "/items/0/id" };
const rev = { $from: "nodes.find.output", path: "/items/0/rev" };

/// 貸出（ボタン）。`hold` を渡すと、探してから状態を見るまでに待ちを挟む（同時押しの確かめ用の版）。
function lendIr(displayName: string, hold?: number): Json {
  const nodes: Json[] = [
    find,
    status([
      { port: "ok", equals: "在庫" },
      { port: "none", equals: "なし" },
    ]),
    missing,
    reply("busy", "貸し出せない", "error", {
      $template: "この備品はいま貸し出せません（{status}）",
      vars: { status: { $from: "nodes.find.output", path: "/items/0/data/status" } },
    }),
    {
      id: "lend",
      type: "data.transition",
      label: "貸出中にする",
      on_error: "continue",
      params: { table: "{{table.equipment}}", record, to: "貸出中", expected_rev: rev },
    },
    {
      id: "set_borrower",
      type: "data.record.update",
      label: "借りた人を書く",
      params: {
        table: "{{table.equipment}}",
        record,
        patch: { borrower: { $from: "run", path: "/started_by" }, due_date: { $from: "input", path: "/due_date" } },
      },
    },
    {
      id: "log",
      type: "data.record.create",
      label: "貸出記録を残す",
      params: {
        table: "{{table.loans}}",
        data: {
          asset_no: { $from: "input", path: "/asset_no" },
          borrower: { $from: "run", path: "/started_by" },
          out_date: { $from: "run", path: "/date" },
          due_date: { $from: "input", path: "/due_date" },
        },
      },
    },
    reply("done", "貸し出した", "success", {
      $template: "{name} を貸し出しました。返却予定日は {due} です",
      vars: { name: { $from: "nodes.find.output", path: "/items/0/data/name" }, due: { $from: "input", path: "/due_date" } },
    }),
    {
      id: "why",
      type: "control.switch",
      label: "貸し出せなかった理由",
      params: { value: { $from: "nodes.lend.output", path: "/error/code" }, cases: [{ port: "conflict", equals: "conflict" }] },
    },
    reply("lost", "先に借りられた", "error", "ほかの人が先に借りました。表を読み直してください"),
    reply("failed", "貸し出せなかった", "error", "貸し出しできませんでした。もう一度押すか、総務に連絡してください"),
  ];
  const edges: Json[] = [
    { from: "state", from_port: "ok", to: "lend" },
    { from: "state", from_port: "none", to: "missing" },
    { from: "state", from_port: "default", to: "busy" },
    { from: "lend", to: "set_borrower" },
    { from: "set_borrower", to: "log" },
    { from: "log", to: "done" },
    { from: "lend", from_port: "error", to: "why" },
    { from: "why", from_port: "conflict", to: "lost" },
    { from: "why", from_port: "default", to: "failed" },
  ];
  if (hold) {
    nodes.splice(1, 0, { id: "hold", type: "control.wait", label: "少し待つ", params: { kind: "duration", duration_sec: hold } });
    edges.unshift({ from: "find", to: "hold" }, { from: "hold", to: "state" });
  } else {
    edges.unshift({ from: "find", to: "state" });
  }
  return {
    ir_version: 1,
    name: "placeholder",
    display_name: displayName,
    description: "資産番号の備品が在庫なら貸出中にし、借りた人と返却予定日を書いて、貸出記録を残す",
    declared_scopes: ["data.read", "data.write"],
    triggers: [{ kind: "interactive" }],
    input_schema: {
      type: "object",
      properties: {
        asset_no: { type: "string", title: "資産番号" },
        due_date: { type: "string", format: "date", title: "返却予定日" },
      },
      required: ["asset_no", "due_date"],
      "x-order": ["asset_no", "due_date"],
    },
    nodes,
    edges,
  };
}

/// 返却（ボタン）。
const backIr: Json = {
  ir_version: 1,
  name: "placeholder",
  display_name: "返却",
  description: "貸出中の備品を在庫に戻し、借りた人と返却予定日を空にして、貸出記録に返却日を入れる",
  declared_scopes: ["data.read", "data.write"],
  triggers: [{ kind: "interactive" }],
  input_schema: {
    type: "object",
    properties: { asset_no: { type: "string", title: "資産番号" } },
    required: ["asset_no"],
    "x-order": ["asset_no"],
  },
  nodes: [
    find,
    status([
      { port: "ok", equals: "貸出中" },
      { port: "none", equals: "なし" },
    ]),
    missing,
    reply("not_lent", "貸出中でない", "error", {
      $template: "この備品は貸出中ではありません（{status}）",
      vars: { status: { $from: "nodes.find.output", path: "/items/0/data/status" } },
    }),
    {
      id: "back",
      type: "data.transition",
      label: "在庫に戻す",
      on_error: "continue",
      params: { table: "{{table.equipment}}", record, to: "在庫", expected_rev: rev },
    },
    {
      id: "clear",
      type: "data.record.update",
      label: "借りた人を空にする",
      params: { table: "{{table.equipment}}", record, patch: { borrower: null, due_date: null } },
    },
    {
      id: "open_loan",
      type: "data.query",
      label: "返却前の貸出記録",
      params: {
        table: "{{table.loans}}",
        query: {
          filter: {
            all: [
              { cmp: { field: "asset_no", op: "eq", value: { $from: "input", path: "/asset_no" } } },
              { cmp: { field: "returned_on", op: "is_null" } },
            ],
          },
          limit: 1,
        },
      },
    },
    {
      id: "close",
      type: "data.record.update",
      label: "返却日を入れる",
      params: {
        table: "{{table.loans}}",
        record: { $from: "nodes.open_loan.output", path: "/items/0/id" },
        patch: { returned_on: { $from: "run", path: "/date" } },
      },
    },
    reply("done", "返却した", "success", {
      $template: "{name} の返却を受け付けました",
      vars: { name: { $from: "nodes.find.output", path: "/items/0/data/name" } },
    }),
    {
      id: "why",
      type: "control.switch",
      label: "返却できなかった理由",
      params: {
        value: { $from: "nodes.back.output", path: "/error/code" },
        cases: [
          { port: "forbidden", equals: "forbidden" },
          { port: "conflict", equals: "conflict" },
        ],
      },
    },
    reply("not_yours", "借りた人でない", "error", "あなたが借りている備品ではありません。借りている人か総務に頼んでください"),
    reply("clash", "重なった", "error", "ほかの操作と重なりました。表を読み直して、もう一度押してください"),
    reply("failed", "返却できなかった", "error", "返却できませんでした。総務に連絡してください"),
  ],
  edges: [
    { from: "find", to: "state" },
    { from: "state", from_port: "ok", to: "back" },
    { from: "state", from_port: "none", to: "missing" },
    { from: "state", from_port: "default", to: "not_lent" },
    { from: "back", to: "clear" },
    { from: "clear", to: "open_loan" },
    { from: "open_loan", to: "close" },
    { from: "close", to: "done" },
    { from: "back", from_port: "error", to: "why" },
    { from: "why", from_port: "forbidden", to: "not_yours" },
    { from: "why", from_port: "conflict", to: "clash" },
    { from: "why", from_port: "default", to: "failed" },
  ],
};

/// 延滞チェックの一覧と印の script（期日切れで未返却の行を全部載せ、まだ印の無い行にだけ印を付ける）。
const OVERDUE_SCRIPT = `function main(input) {
  var items = input.items || [];
  var lines = items.map(function (r) {
    return "- " + r.data.asset_no + "（返却予定日 " + r.data.due_date + "・延滞の記録 " + (r.data.overdue_on || input.date) + "）";
  });
  var fresh = items.filter(function (r) { return !r.data.overdue_on; });
  return {
    text: "# 延滞の一覧（" + input.date + "）\\n\\n" + (lines.length ? lines.join("\\n") : "延滞はありません。") + "\\n",
    rows: fresh.map(function (r) { return { id: r.id, rev: r.rev, data: { overdue_on: input.date } }; }),
    count: fresh.length
  };
}`;

/// 延滞チェック（毎朝 9 時）。印を付ける行が無い日は一括の書き込みを飛ばす（行が 0 件の data.bulk_upsert は
/// 「取り込む行がありません」で落ちるため・findings を参照）。
const overdueIr: Json = {
  ir_version: 1,
  name: "placeholder",
  display_name: "延滞チェック",
  description: "返却予定日を過ぎて返っていない貸出記録の一覧を総務のフォルダーに置き、延滞の印を付ける",
  declared_scopes: ["data.read", "data.write", "storage.write"],
  triggers: [{ kind: "schedule", cron: "0 9 * * *", tz: "Asia/Tokyo", catchup: "skip" }],
  nodes: [
    { id: "today", type: "transform.date", label: "今日", params: {} },
    {
      id: "late",
      type: "data.query",
      label: "延滞している記録",
      params: {
        table: "{{table.loans}}",
        query: {
          filter: {
            all: [
              { cmp: { field: "due_date", op: "lt", value: { $from: "nodes.today.output", path: "/date" } } },
              { cmp: { field: "returned_on", op: "is_null" } },
            ],
          },
          sort: [{ field: "due_date" }],
          limit: 200,
        },
      },
    },
    {
      id: "fmt",
      type: "script.run",
      label: "一覧と印",
      params: {
        source: { inline: OVERDUE_SCRIPT },
        input: { items: { $from: "nodes.late.output", path: "/items" }, date: { $from: "nodes.today.output", path: "/date" } },
      },
    },
    {
      id: "save",
      type: "storage.write",
      label: "一覧を保存",
      params: {
        folder: "{{folder.soumu}}",
        name: { $template: "延滞一覧-{date}.md", vars: { date: { $from: "nodes.today.output", path: "/date" } } },
        content: { $from: "nodes.fmt.output", path: "/text" },
        content_type: "text/markdown",
        on_name_taken: "add_number",
      },
    },
    {
      id: "any",
      type: "control.branch",
      label: "新しい延滞があるか",
      params: { condition: { cmp: { left: { $from: "nodes.fmt.output", path: "/count" }, op: "gt", right: 0 } } },
    },
    {
      id: "mark",
      type: "data.bulk_upsert",
      label: "延滞の印を付ける",
      params: { table: "{{table.loans}}", rows: { $from: "nodes.fmt.output", path: "/rows" } },
    },
  ],
  edges: [
    { from: "today", to: "late" },
    { from: "late", to: "fmt" },
    { from: "fmt", to: "save" },
    { from: "save", to: "any" },
    { from: "any", from_port: "true", to: "mark" },
  ],
};

const MAIN_SCREEN = `# 備品の貸出

資産番号は、備品に貼ったラベルに書いてあります。

\`\`\`ui
app 備品の貸出
## 借りる
資産番号 = text 必須 as asset_no
返却予定日 = date 必須 as due_date
button 借りる -> workflow 貸出
## 返す
資産番号 = text 必須 as asset_no
button 返す -> workflow 返却
\`\`\`

## 貸出できる備品

\`\`\`data_grid
貸出できる備品
\`\`\`

## 自分の貸出中

\`\`\`data_grid
自分の貸出中
\`\`\`
`;

const WAIT_SCREEN = `# 備品の貸出（同時操作の確かめ）

\`\`\`ui
app 備品の貸出（待ちを挟む版）
資産番号 = text 必須 as asset_no
返却予定日 = date 必須 as due_date
button 借りる -> workflow 貸出（待ち）
\`\`\`
`;

const LOAN_FIELDS = [
  { key: "asset_no", display_name: "資産番号", type: "text", indexed: true },
  { key: "borrower", display_name: "借りた人", type: "user_ref", indexed: true },
  { key: "out_date", display_name: "貸出日", type: "date" },
  { key: "due_date", display_name: "返却予定日", type: "date", indexed: true },
  { key: "returned_on", display_name: "返却日", type: "date", indexed: true },
  { key: "overdue_on", display_name: "延滞の記録", type: "date", indexed: true },
];

/// 貸出記録の seed（borrower は人の名前 → `{{user.<名前>}}`）。
function loanRow(l: { asset_no: string; borrower: string; out_date: string; due_date: string; returned_on?: string }): Json {
  return { ...l, borrower: `{{user.${l.borrower}}}` };
}

/// APP-11 の世界の定義。
export function app11World(): WorldDef {
  return {
    key: "app11",
    roles: [{ ref: "soumu", members: ["alice"] }],
    folders: [{ ref: "soumu", name: "総務-延滞一覧" }],
    tables: [
      {
        ref: "equipment",
        name: "備品",
        schema: {
          fields: [
            { key: "asset_no", display_name: "資産番号", type: "text", unique: true, indexed: true },
            { key: "name", display_name: "品名", type: "text" },
            { key: "category", display_name: "区分", type: "select", options: ["PC", "プロジェクター", "カメラ", "その他"] },
            { key: "borrower", display_name: "借りている人", type: "user_ref", indexed: true },
            { key: "due_date", display_name: "返却予定日", type: "date", indexed: true },
            { key: "status", display_name: "状態", type: "select", options: ["在庫", "貸出中", "修理中"], indexed: true },
          ],
          status_field: "status",
        },
        fsm: {
          status_field: "status",
          states: ["在庫", "貸出中", "修理中"],
          transitions: [
            // 遷移の時点では借りた人の列が空なので、本人と比べられない（だれでも貸出中にできる・台帳の risk）。
            { from: "在庫", to: "貸出中", actor: "public" },
            { from: "貸出中", to: "在庫", actor: { any: [BORROWER, ROLE] } },
            { from: "在庫", to: "修理中", actor: ROLE },
            { from: "修理中", to: "在庫", actor: ROLE },
            { from: "貸出中", to: "修理中", actor: ROLE },
          ],
        },
        rows: [
          { asset_no: ASSET.race, name: "プロジェクター 1 号機", category: "プロジェクター", status: "在庫" },
          { asset_no: ASSET.raceWait, name: "ノート PC 2 号機", category: "PC", status: "在庫" },
          { asset_no: ASSET.twice, name: "デジタルカメラ 3 号機", category: "カメラ", status: "在庫" },
          { asset_no: ASSET.bobUi, name: "ノート PC 4 号機", category: "PC", status: "在庫" },
          { asset_no: ASSET.alices, name: "延長コード 5 号", category: "その他", status: "在庫" },
        ],
        shares: [{ user: "bob", role: "editor" }],
      },
      {
        ref: "loans",
        name: "貸出記録",
        schema: {
          fields: LOAN_FIELDS,
          row_policy: { read: { any: [BORROWER, ROLE] }, write: { any: [BORROWER, ROLE] } },
        },
        rows: Object.values(LOAN).map(loanRow),
        shares: [{ user: "bob", role: "editor" }],
      },
    ],
    views: [
      {
        ref: "available",
        name: "貸出できる備品",
        table: "equipment",
        query: { filter: { cmp: { field: "status", op: "eq", value: "在庫" } }, sort: [{ field: "asset_no" }] },
      },
      {
        ref: "mine",
        name: "自分の貸出中",
        table: "loans",
        // ビューの条件に「自分」は書けない（$user.id はリテラルでない）。本人の分への絞り込みは行の決まりで行う。
        query: { filter: { cmp: { field: "returned_on", op: "is_null" } }, sort: [{ field: "due_date" }] },
      },
    ],
    workflows: [
      { ref: "lend", ir: lendIr("貸出") },
      { ref: "back", ir: backIr },
      { ref: "lend_wait", ir: lendIr("貸出（待ち）", 4) },
      {
        ref: "overdue",
        ir: overdueIr,
        enable: {
          grants: [
            { scope: "data.read", kind: "data_table", target: "{{table.loans}}", relation: "viewer" },
            { scope: "data.write", kind: "data_table", target: "{{table.loans}}", relation: "editor" },
            { scope: "storage.write", kind: "folder", target: "{{folder.soumu}}", relation: "editor" },
          ],
        },
      },
    ],
    apps: [
      {
        ref: "main",
        name: "備品の貸出",
        description: "備品を借りる・返す。貸出できる備品と自分の貸出中を表で見る",
        screen: MAIN_SCREEN,
        workflows: [
          { alias: "貸出", wf: "lend" },
          { alias: "返却", wf: "back" },
        ],
        views: [
          { alias: "貸出できる備品", view: "available" },
          { alias: "自分の貸出中", view: "mine" },
        ],
        shares: [{ user: "bob", role: "viewer" }],
      },
      {
        ref: "wait",
        name: "備品の貸出（待ちを挟む版）",
        description: "同時に借りたときの rev の競合を確かめる版（探してから遷移までに待ちを挟む）",
        screen: WAIT_SCREEN,
        workflows: [{ alias: "貸出（待ち）", wf: "lend_wait" }],
        shares: [{ user: "bob", role: "viewer" }],
      },
    ],
  };
}
