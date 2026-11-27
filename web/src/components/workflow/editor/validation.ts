/// サーバ検証エラー（全件・node_id/edge/path 付き）のノード/エッジへの写像。
///
/// backend は `{ code, message, node_id?, edge?("from -> to"), path?("/params/...") }` を
/// 全件返す（ir.md §8）。dnd は該当ノード上にバッジ表示し、右パネルはフィールドへ写像する。

import type { Trigger, ValidationError } from "@/generated/workflow-ir";

export type ErrorMap = {
  byNode: Map<string, ValidationError[]>;
  byEdge: Map<string, ValidationError[]>;
  /// ブロックにも線にも紐付かない誤り（下の 3 つを合わせたもの）。
  global: ValidationError[];
  /// きっかけの誤り（path が `/triggers/<i>/…`・添字ごと）。きっかけの設定の欄の下に出す。
  byTrigger: Map<number, ValidationError[]>;
  /// 入力項目の誤り（path が `/input_schema…`）。入力項目の編集の下に出す。
  inputSchema: ValidationError[];
  /// どこにも写せない誤り（ワークフロー全体）。誤りの一覧にだけ出す。
  other: ValidationError[];
};

const TRIGGER_PATH = /^\/triggers\/(\d+)(?:\/|$)/;

export function mapErrors(errors: ValidationError[]): ErrorMap {
  const byNode = new Map<string, ValidationError[]>();
  const byEdge = new Map<string, ValidationError[]>();
  const byTrigger = new Map<number, ValidationError[]>();
  const global: ValidationError[] = [];
  const inputSchema: ValidationError[] = [];
  const other: ValidationError[] = [];
  for (const e of errors) {
    if (e.node_id) {
      const list = byNode.get(e.node_id) ?? [];
      list.push(e);
      byNode.set(e.node_id, list);
      continue;
    }
    if (e.edge) {
      const list = byEdge.get(e.edge) ?? [];
      list.push(e);
      byEdge.set(e.edge, list);
      continue;
    }
    global.push(e);
    const trigger = TRIGGER_PATH.exec(e.path ?? "");
    if (trigger) {
      const index = Number(trigger[1]);
      const list = byTrigger.get(index) ?? [];
      list.push(e);
      byTrigger.set(index, list);
    } else if (e.path === "/input_schema" || e.path?.startsWith("/input_schema/")) {
      inputSchema.push(e);
    } else {
      other.push(e);
    }
  }
  return { byNode, byEdge, global, byTrigger, inputSchema, other };
}

/// きっかけ 1 つの保存時の誤り（入力項目の誤りは「手動で実行」のきっかけの設定で直すので、そこにも数える）。
export function triggerErrorsOf(map: ErrorMap, trigger: Trigger, index: number): ValidationError[] {
  const own = map.byTrigger.get(index) ?? [];
  if (trigger.kind !== "interactive" || map.inputSchema.length === 0) return own;
  return [...own, ...map.inputSchema];
}

/// きっかけの設定にある欄（誤りを欄の下に出せる位置）。
const FIELD_KEYS = new Set(["scope/folder", "scope/table", "cron", "tz"]);

/// きっかけの誤りの文言の頭（どのきっかけかは場所の名前で別に言うので、画面では外す）。
const TRIGGER_PREFIX = "きっかけ: ";

/// きっかけの誤りの文言から、頭の「きっかけ: 」を外す（場所はきっかけの設定・一覧の場所の名前が言う）。
export function withoutTriggerPrefix(message: string): string {
  return message.startsWith(TRIGGER_PREFIX) ? message.slice(TRIGGER_PREFIX.length) : message;
}

/// きっかけ 1 つの誤りを、欄の下に出すもの（`scope/folder`・`scope/table`・`cron`・`tz` など、きっかけの中の
/// 位置 → 文言）と、上の帯に出すもの（欄に写せないもの）に分ける。
export function triggerErrorView(errors: ValidationError[]): {
  fields: Map<string, string>;
  banner: string[];
} {
  const fields = new Map<string, string>();
  const banner: string[] = [];
  for (const e of errors) {
    const message = withoutTriggerPrefix(e.message);
    const rest = (e.path ?? "").replace(TRIGGER_PATH, "");
    const [head, sub] = rest.split("/");
    const field = head === "scope" && sub ? `scope/${sub}` : head || null;
    if (field && FIELD_KEYS.has(field)) {
      if (!fields.has(field)) fields.set(field, message);
    } else if (!banner.includes(message)) {
      banner.push(message);
    }
  }
  return { fields, banner };
}

/// 保存の失敗の知らせで、直すところを言う文（ブロックの誤りが無いときは場所を言う・付録 C の P5）。
export function saveErrorHint(errors: ValidationError[]): string {
  const map = mapErrors(errors);
  const places: string[] = [];
  if (map.byNode.size > 0 || map.byEdge.size > 0) places.push("赤い印のブロックや線");
  if (map.byTrigger.size > 0) places.push("きっかけの設定");
  if (map.inputSchema.length > 0) places.push("入力項目");
  if (map.other.length > 0) places.push("ワークフロー全体の設定");
  if (places.length === 0) return "入力した内容を確認してください。";
  if (places.length === 1 && places[0] === "赤い印のブロックや線") {
    return "赤い印のブロックや線を確認してください。";
  }
  return `${places.join("・")}に直すところがあります。上の「直すところ」から開けます。`;
}

/// サーバの params の読み取りの誤り（`params が不正です: 項目 'data' がありません` など）から、
/// 欄の名前と誤りの種類を取り出す。サーバは serde の英語の文言を日本語に直して返す
/// （`params/mod.rs` の humanize・BUG-21）。英語の原文（`missing field \`data\``）は古いサーバ向けに残す。
export function serdeParamError(
  message: string,
): { field: string; kind: "missing" | "unknown" | "invalid"; message: string } | null {
  const missing = /項目 '([^']+)' がありません/.exec(message) ?? /missing field `([^`]+)`/.exec(message);
  if (missing) return { field: missing[1], kind: "missing", message: "入力してください" };
  const unknown = /項目 '([^']+)' は使えません/.exec(message) ?? /unknown field `([^`]+)`/.exec(message);
  if (unknown) {
    return { field: unknown[1], kind: "unknown", message: `「${unknown[1]}」はこのブロックでは使えない項目です` };
  }
  const invalid = /invalid (?:type|value)[^`]*?(?:at|for) `?([a-z_][a-z0-9_]*)`?/.exec(message);
  if (invalid) return { field: invalid[1], kind: "invalid", message: "値の形が正しくありません" };
  return null;
}

/// 右パネルでの誤りの出し分け。
export type NodeErrorView = {
  /// 欄の下に出す誤り（params の最上位の項目名 → 文言・同じ欄の 2 件目以降は捨てる）。
  fields: Map<string, string>;
  /// 上の帯に出す誤り（欄に写せないもの）。`removable` は params の最上位にある使えない項目で、
  /// 帯から取り除ける（フォームはその項目の欄を描かないので、ほかに消す手段が無い）。`removableAttr` は
  /// params の外の step 属性（`cache`）の誤りで、帯から属性ごと外せる（付けられない種には欄が無い）。
  banner: { message: string; removable?: string; removableAttr?: "cache" }[];
  /// params の外の step 属性の欄（共通の設定の「時間の上限」＝ `timeout_sec`）の下に出す誤り（BUG-96）。
  attrs: Map<string, string>;
};

/// 共通の設定に欄がある step 属性（path が `/<属性>` の誤りはその欄の下へ）。
const ATTR_FIELDS = ["timeout_sec"] as const;

const PARAMS_PREFIX = "params が不正です: ";

/// 1 つのノードの誤りを、欄の下に出すものと上の帯に出すものに分ける。
///
/// - path が `/params/<項目>/…` なら、その項目の欄へ（文言の `params が不正です: ` は欄の下では外す）。
/// - path が `/params` だけの欠落（`項目 'x' がありません`）は、文言の項目名の欄へ「入力してください」。
/// - 最上位の使えない項目（path が `/params/<項目>` で文言も同じ項目）は帯へ（取り除けるようにする）。
export function nodeErrorView(errors: ValidationError[]): NodeErrorView {
  const fields = new Map<string, string>();
  const banner: NodeErrorView["banner"] = [];
  const attrs = new Map<string, string>();
  for (const e of errors) {
    const attr = ATTR_FIELDS.find((a) => e.path === `/${a}`);
    if (attr) {
      if (!attrs.has(attr)) attrs.set(attr, e.message);
      continue;
    }
    // step 属性 `cache`（決定キャッシュ・付けられるのは ai.decide だけ）の誤りは帯へ。外せばいつでも直る。
    if (e.path === "/cache" || e.path?.startsWith("/cache/")) {
      banner.push({ message: e.message, removableAttr: "cache" });
      continue;
    }
    const head = paramField(e.path);
    // 文言から項目名を拾うのは params の読み取りの誤り（path が無いか `/params`）だけ。
    const serde = !e.path || e.path === "/params" || head !== null ? serdeParamError(e.message) : null;
    if (serde?.kind === "unknown" && (head === null || head === serde.field)) {
      banner.push({ message: serde.message, removable: serde.field });
      continue;
    }
    const field = head ?? serde?.field ?? null;
    if (!field) {
      banner.push({ message: e.message });
      continue;
    }
    const message =
      head === null && serde
        ? serde.message
        : e.message.startsWith(PARAMS_PREFIX)
          ? e.message.slice(PARAMS_PREFIX.length)
          : e.message;
    // 入れ子の欄（`/params/query/limit` → `query/limit`）にも写す。1 つの項目の中に欄が複数あるフォーム
    // （「データを調べる」の探す条件と読む件数の上限）が、直す欄の下に出せるように（BUG-108）。
    const sub = paramSubField(e.path);
    if (sub && !fields.has(sub)) fields.set(sub, message);
    if (!fields.has(field)) fields.set(field, message);
  }
  return { fields, banner, attrs };
}

/// `/params/<field>/<sub>/...` の 2 段の名前（`<field>/<sub>`）。2 段に満たなければ null。
export function paramSubField(path: string | undefined | null): string | null {
  if (!path?.startsWith("/params/")) return null;
  const [head, sub] = path.slice("/params/".length).split("/");
  return head && sub ? `${head}/${sub}` : null;
}

/// `/params/<field>/...` の先頭フィールド名（右パネルのフィールドハイライト用）。
export function paramField(path: string | undefined | null): string | null {
  if (!path?.startsWith("/params/")) return null;
  const rest = path.slice("/params/".length);
  const head = rest.split("/")[0];
  return head || null;
}
