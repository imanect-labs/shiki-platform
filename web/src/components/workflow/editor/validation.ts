/// サーバ検証エラー（全件・node_id/edge/path 付き）のノード/エッジへの写像。
///
/// backend は `{ code, message, node_id?, edge?("from -> to"), path?("/params/...") }` を
/// 全件返す（ir.md §8）。dnd は該当ノード上にバッジ表示し、右パネルはフィールドへ写像する。

import type { ValidationError } from "@/generated/workflow-ir";

export type ErrorMap = {
  byNode: Map<string, ValidationError[]>;
  byEdge: Map<string, ValidationError[]>;
  global: ValidationError[];
};

export function mapErrors(errors: ValidationError[]): ErrorMap {
  const byNode = new Map<string, ValidationError[]>();
  const byEdge = new Map<string, ValidationError[]>();
  const global: ValidationError[] = [];
  for (const e of errors) {
    if (e.node_id) {
      const list = byNode.get(e.node_id) ?? [];
      list.push(e);
      byNode.set(e.node_id, list);
    } else if (e.edge) {
      const list = byEdge.get(e.edge) ?? [];
      list.push(e);
      byEdge.set(e.edge, list);
    } else {
      global.push(e);
    }
  }
  return { byNode, byEdge, global };
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
