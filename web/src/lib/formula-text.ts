/// 数式の列（`formula`）の文字の書き方 ⇔ サーバの閉じた AST（`FormulaExpr`）。
///
/// スキーマエディタで数式を文字で書けるようにするための純粋な変換（A-5）。AST の語彙・型の検査・評価は
/// サーバが正（ここは書き方を AST に組むだけで、列の存在・型の合否はサーバが構造化 400 で返す）。
///
/// 書き方:
/// - 列: 参照名（`price`）か、表示名を「」で囲む（`「単価」`）。表示名は呼び出し側が参照名へ解決する
/// - 値: 数値（`1.5`）・文字列（`"円"`）・`now()`
/// - 演算: `+ - * /`（`+` は数値の足し算。文字の連結は `concat(a, b)`）・比較 `= != < <= > >=`・`and` `or` `not`
/// - 関数: `round(x, 桁)`・`concat(…)`・`upper(x)`・`lower(x)`・`date_add(日付, 日数)`・`date_diff(a, b)`・
///   `if(条件, 真, 偽)`
import type { components } from "@/generated/api";

export type FormulaExpr = components["schemas"]["FormulaExpr"];
type CmpOp = components["schemas"]["FormulaCmpOp"];

type Tok =
  | { k: "num"; v: number }
  | { k: "str"; v: string }
  | { k: "id"; v: string }
  | { k: "label"; v: string }
  | { k: "op"; v: string }
  | { k: "(" }
  | { k: ")" }
  | { k: "," };

export class FormulaSyntaxError extends Error {}

function tokenize(src: string): Tok[] {
  const out: Tok[] = [];
  let i = 0;
  while (i < src.length) {
    const c = src[i];
    if (/\s/.test(c)) {
      i += 1;
      continue;
    }
    if (c === "(" || c === ")" || c === ",") {
      out.push({ k: c } as Tok);
      i += 1;
      continue;
    }
    if (c === "「") {
      const j = src.indexOf("」", i + 1);
      if (j < 0) throw new FormulaSyntaxError("「 に対応する 」がありません");
      out.push({ k: "label", v: src.slice(i + 1, j) });
      i = j + 1;
      continue;
    }
    if (c === '"') {
      let j = i + 1;
      let v = "";
      while (j < src.length && src[j] !== '"') {
        if (src[j] === "\\" && j + 1 < src.length) j += 1;
        v += src[j];
        j += 1;
      }
      if (j >= src.length) throw new FormulaSyntaxError('" に対応する " がありません');
      out.push({ k: "str", v });
      i = j + 1;
      continue;
    }
    const two = src.slice(i, i + 2);
    if (two === "<=" || two === ">=" || two === "!=" || two === "==") {
      out.push({ k: "op", v: two === "==" ? "=" : two });
      i += 2;
      continue;
    }
    if ("+-*/<>=".includes(c)) {
      out.push({ k: "op", v: c });
      i += 1;
      continue;
    }
    const num = /^\d+(\.\d+)?/.exec(src.slice(i));
    if (num) {
      out.push({ k: "num", v: Number(num[0]) });
      i += num[0].length;
      continue;
    }
    const id = /^[A-Za-z_][A-Za-z0-9_]*/.exec(src.slice(i));
    if (id) {
      out.push({ k: "id", v: id[0] });
      i += id[0].length;
      continue;
    }
    throw new FormulaSyntaxError(`使えない文字です: ${c}`);
  }
  return out;
}

const CMP: Record<string, CmpOp> = { "=": "eq", "!=": "ne", "<": "lt", "<=": "lte", ">": "gt", ">=": "gte" };

/// 文字の数式 → AST。`resolveLabel` は「表示名」→ 参照名（見つからなければ null）。
export function parseFormula(src: string, resolveLabel: (label: string) => string | null): FormulaExpr {
  const toks = tokenize(src);
  let p = 0;
  const peek = () => toks[p];
  const eat = (k: Tok["k"], v?: string) => {
    const t = toks[p];
    if (!t || t.k !== k || (v !== undefined && (t as { v?: string }).v !== v)) return false;
    p += 1;
    return true;
  };
  const expect = (k: Tok["k"], what: string) => {
    if (!eat(k)) throw new FormulaSyntaxError(`${what}がありません`);
  };
  const isWord = (w: string) => {
    const t = peek();
    return t?.k === "id" && t.v.toLowerCase() === w;
  };

  const orExpr = (): FormulaExpr => {
    let left = andExpr();
    const items = [left];
    while (isWord("or")) {
      p += 1;
      items.push(andExpr());
    }
    if (items.length > 1) left = { or: items };
    return left;
  };
  const andExpr = (): FormulaExpr => {
    let left = notExpr();
    const items = [left];
    while (isWord("and")) {
      p += 1;
      items.push(notExpr());
    }
    if (items.length > 1) left = { and: items };
    return left;
  };
  const notExpr = (): FormulaExpr => {
    if (isWord("not") && toks[p + 1]?.k !== "(") {
      p += 1;
      return { not: notExpr() };
    }
    return cmpExpr();
  };
  const cmpExpr = (): FormulaExpr => {
    const left = addExpr();
    const t = peek();
    if (t?.k === "op" && CMP[t.v]) {
      p += 1;
      const right = addExpr();
      return { cmp: [left, CMP[t.v], right] };
    }
    return left;
  };
  const addExpr = (): FormulaExpr => {
    let left = mulExpr();
    for (;;) {
      const t = peek();
      if (t?.k === "op" && (t.v === "+" || t.v === "-")) {
        p += 1;
        const right = mulExpr();
        left = t.v === "+" ? { add: [left, right] } : { sub: [left, right] };
      } else return left;
    }
  };
  const mulExpr = (): FormulaExpr => {
    let left = unary();
    for (;;) {
      const t = peek();
      if (t?.k === "op" && (t.v === "*" || t.v === "/")) {
        p += 1;
        const right = unary();
        left = t.v === "*" ? { mul: [left, right] } : { div: [left, right] };
      } else return left;
    }
  };
  const unary = (): FormulaExpr => {
    const t = peek();
    if (t?.k === "op" && t.v === "-") {
      p += 1;
      const inner = unary();
      if ("lit" in (inner as object) && typeof (inner as { lit: unknown }).lit === "number") {
        return { lit: -((inner as { lit: number }).lit) };
      }
      return { sub: [{ lit: 0 }, inner] };
    }
    return primary();
  };
  const args = (): FormulaExpr[] => {
    expect("(", "「(」");
    const out: FormulaExpr[] = [];
    if (eat(")")) return out;
    for (;;) {
      out.push(orExpr());
      if (eat(")")) return out;
      expect(",", "「,」か「)」");
    }
  };
  const primary = (): FormulaExpr => {
    const t = peek();
    if (!t) throw new FormulaSyntaxError("式が途中で終わっています");
    if (t.k === "num") {
      p += 1;
      return { lit: t.v };
    }
    if (t.k === "str") {
      p += 1;
      return { lit: t.v };
    }
    if (t.k === "label") {
      p += 1;
      const key = resolveLabel(t.v);
      if (!key) throw new FormulaSyntaxError(`列「${t.v}」がありません`);
      return { field: key };
    }
    if (t.k === "(") {
      p += 1;
      const e = orExpr();
      expect(")", "「)」");
      return e;
    }
    if (t.k === "id") {
      p += 1;
      const name = t.v.toLowerCase();
      if (peek()?.k !== "(") {
        if (name === "true" || name === "false") return { lit: name === "true" };
        return { field: t.v };
      }
      const a = args();
      const need = (n: number) => {
        if (a.length !== n) throw new FormulaSyntaxError(`${name} の引数は ${n} 個です`);
      };
      switch (name) {
        case "now":
          need(0);
          return "now";
        case "round": {
          if (a.length !== 1 && a.length !== 2) throw new FormulaSyntaxError("round の引数は 1 個か 2 個です");
          const digits = a[1];
          if (digits && !(typeof digits === "object" && "lit" in digits && typeof digits.lit === "number")) {
            throw new FormulaSyntaxError("round の桁数は数値で書いてください");
          }
          return { round: digits ? [a[0], (digits as { lit: number }).lit] : [a[0], 0] };
        }
        case "concat":
          return { concat: a };
        case "upper":
          need(1);
          return { upper: a[0] };
        case "lower":
          need(1);
          return { lower: a[0] };
        case "date_add":
          need(2);
          return { date_add: a };
        case "date_diff":
          need(2);
          return { date_diff: a };
        case "if":
          need(3);
          return { if: a };
        case "and":
          return { and: a };
        case "or":
          return { or: a };
        case "not":
          need(1);
          return { not: a[0] };
        default:
          throw new FormulaSyntaxError(`知らない関数です: ${t.v}`);
      }
    }
    throw new FormulaSyntaxError("式の書き方が正しくありません");
  };

  if (toks.length === 0) throw new FormulaSyntaxError("式を入れてください");
  const e = orExpr();
  if (p < toks.length) throw new FormulaSyntaxError("式の書き方が正しくありません（余分な文字があります）");
  return e;
}

const CMP_TEXT: Record<CmpOp, string> = { eq: "=", ne: "!=", lt: "<", lte: "<=", gt: ">", gte: ">=" };

/// AST → 文字の数式（既存の数式を編集欄に出す）。`labelOf` は参照名 → 表示名（あれば「」で囲んで出す）。
export function formatFormula(e: FormulaExpr, labelOf: (key: string) => string | null = () => null): string {
  const f = (x: FormulaExpr): string => formatFormula(x, labelOf);
  const bin = (xs: FormulaExpr[], op: string) => `(${xs.map(f).join(` ${op} `)})`;
  if (e === "now") return "now()";
  if ("field" in e) {
    const l = labelOf(e.field);
    return l ? `「${l}」` : e.field;
  }
  if ("lit" in e) return typeof e.lit === "string" ? JSON.stringify(e.lit) : String(e.lit);
  if ("add" in e) return bin(e.add, "+");
  if ("sub" in e) return bin(e.sub, "-");
  if ("mul" in e) return bin(e.mul, "*");
  if ("div" in e) return bin(e.div, "/");
  if ("round" in e) return `round(${e.round.map((x) => (typeof x === "number" ? String(x) : f(x))).join(", ")})`;
  if ("concat" in e) return `concat(${e.concat.map(f).join(", ")})`;
  if ("upper" in e) return `upper(${f(e.upper)})`;
  if ("lower" in e) return `lower(${f(e.lower)})`;
  if ("date_add" in e) return `date_add(${e.date_add.map(f).join(", ")})`;
  if ("date_diff" in e) return `date_diff(${e.date_diff.map(f).join(", ")})`;
  if ("if" in e) return `if(${e.if.map(f).join(", ")})`;
  if ("and" in e) return `and(${e.and.map(f).join(", ")})`;
  if ("or" in e) return `or(${e.or.map(f).join(", ")})`;
  if ("not" in e) return `not(${f(e.not)})`;
  if ("cmp" in e) {
    const [a, op, b] = e.cmp;
    return `(${f(a as FormulaExpr)} ${CMP_TEXT[op as CmpOp] ?? op} ${f(b as FormulaExpr)})`;
  }
  // 文字で書けない形（switch・rollup）は JSON のまま見せる（編集は JSON の書き換えで行う）。
  return JSON.stringify(e);
}
