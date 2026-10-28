"use client";

/// SQL 画面（#978）の表示部品: 結果の表・拒否の理由と位置・書き方の早見。状態は持たない。

import { BookOpen, ChevronDown, CircleAlert } from "lucide-react";
import * as React from "react";

import { DataApiError, type QueryRejection, type SqlQueryResult } from "@/lib/data-api";
import { cn } from "@/lib/utils";

const REJECT_TITLE: Record<string, string> = {
  syntax_error: "SQL として読めません",
  unsupported: "使えない書き方です",
  unknown_name: "名前が見つかりません",
  invalid_join: "結合の書き方が違います",
  invalid_query: "問い合わせの組み立てが合いません",
  limit_exceeded: "上限を超えました",
};

/// 表のセルと同じ精度（数値列は小数 9 桁まで保存できる。lib/data-view と揃える）。
export const numberFmt = new Intl.NumberFormat("ja-JP", { maximumFractionDigits: 10 });

function cellText(v: unknown): string {
  if (v === null || v === undefined) return "";
  if (Array.isArray(v)) return v.map(cellText).join(", ");
  if (typeof v === "number") return numberFmt.format(v);
  if (typeof v === "boolean") return v ? "はい" : "いいえ";
  return String(v);
}

/// `line`・`column`（1 始まり・文字単位）を文字列の位置へ。
export function offsetOf(text: string, line: number, column: number): number {
  const lines = text.split("\n");
  let off = 0;
  for (let i = 0; i < Math.min(line - 1, lines.length); i++) off += lines[i].length + 1;
  return Math.min(off + Math.max(column - 1, 0), text.length);
}

/// 拒否文の名前（`'…'`）。候補をこの名前に近い順に並べるのに使う。
function rejectedName(r: QueryRejection): string | null {
  return /'([^']+)'/.exec(r.message)?.[1] ?? null;
}

function editDistance(a: string, b: string): number {
  const dp = Array.from({ length: b.length + 1 }, (_, j) => j);
  for (let i = 1; i <= a.length; i++) {
    let prev = dp[0];
    dp[0] = i;
    for (let j = 1; j <= b.length; j++) {
      const tmp = dp[j];
      dp[j] = Math.min(dp[j] + 1, dp[j - 1] + 1, prev + (a[i - 1] === b[j - 1] ? 0 : 1));
      prev = tmp;
    }
  }
  return dp[b.length];
}

/// 候補を、拒否された名前に近い順（システム列は後ろ）に並べる。
function rankCandidates(r: QueryRejection): string[] {
  const name = rejectedName(r)?.toLowerCase();
  const list = [...(r.expected ?? [])];
  if (!name) return list;
  return list
    .map((c, i) => ({ c, i, d: editDistance(name, c.toLowerCase()), sys: c.slice(c.lastIndexOf(".") + 1).startsWith("_") }))
    .sort((a, b) => Number(a.sys) - Number(b.sys) || a.d - b.d || a.i - b.i)
    .map((x) => x.c);
}

/// 拒否の位置の行と印。印の前は同じ文字を見えなくして置く（全角・タブでも位置が合う）。
function LocationView({ sql, line, column }: { sql: string; line: number; column: number }) {
  const text = sql.split("\n")[line - 1];
  if (text === undefined) return null;
  const gutter = String(line);
  return (
    <div className="overflow-x-auto rounded-md border bg-background/70 px-3 py-2 scrollbar-subtle" data-testid="sql-error-location">
      <pre className="font-mono text-[12.5px] leading-5 text-foreground">
        <span className="select-none text-muted-foreground">{gutter} │ </span>
        {text}
        {"\n"}
        <span className="invisible select-none">{gutter}</span>
        <span className="select-none text-muted-foreground"> │ </span>
        <span className="invisible select-none">{text.slice(0, Math.max(column - 1, 0))}</span>
        <span className="font-bold text-destructive">^</span>
      </pre>
    </div>
  );
}

export function SqlErrorPanel({
  error,
  ranSql,
  replaceable,
  onCandidate,
}: {
  error: unknown;
  /// 拒否された SQL（入力欄が書き換わっても、位置の表示はこれで出す）。
  ranSql: string;
  /// 候補を押したとき、誤った名前を置き換えられる（入力欄が拒否された SQL のまま）。
  replaceable: boolean;
  onCandidate: (candidate: string) => void;
}) {
  const rejection: QueryRejection | null = error instanceof DataApiError ? error.rejection : null;
  const title = rejection
    ? (REJECT_TITLE[rejection.code] ?? "受け付けられない問い合わせです")
    : error instanceof DataApiError && error.readTimeout
      ? "時間内に読み終わりませんでした"
      : "実行できませんでした";
  const message = rejection?.message ?? (error instanceof Error ? error.message : String(error));
  // 文全体を拒む誤り（DML など）は 1 文字目を指すだけなので、位置を出さない。
  const loc = rejection?.location && !(rejection.code === "unsupported" && rejection.location.column <= 1) ? rejection.location : null;
  const candidates = rejection ? rankCandidates(rejection).slice(0, 40) : [];
  return (
    <div className="space-y-2.5 rounded-xl border border-destructive/40 bg-destructive/5 p-4" role="alert" data-testid="sql-error">
      <div className="flex items-start gap-2">
        <CircleAlert className="mt-0.5 size-4 shrink-0 text-destructive" aria-hidden />
        <div className="min-w-0 space-y-0.5">
          <p className="text-sm font-semibold">{title}</p>
          <p className="break-words text-sm text-foreground/90">{message}</p>
          {loc ? (
            <p className="text-xs text-muted-foreground">
              {loc.line} 行 {loc.column} 文字目
            </p>
          ) : null}
        </div>
      </div>
      {loc ? <LocationView sql={ranSql} line={loc.line} column={loc.column} /> : null}
      {rejection?.hint ? <p className="text-xs text-muted-foreground">手がかり: {rejection.hint}</p> : null}
      {candidates.length > 0 ? (
        <div className="space-y-1">
          <p className="text-xs text-muted-foreground">
            {replaceable ? "使える候補（押すと誤った名前を置き換えます）" : "使える候補（押すとカーソル位置に入ります）"}
          </p>
          <div className="flex flex-wrap gap-1.5" data-testid="sql-error-expected">
            {candidates.map((c, i) => (
              <button
                key={c}
                type="button"
                onClick={() => onCandidate(c)}
                className={cn(
                  "rounded-md border bg-background px-2 py-0.5 font-mono text-[12px] transition-colors duration-fast hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                  i === 0 && rejectedName(rejection!) && "border-primary/50 text-primary",
                )}
              >
                {c}
              </button>
            ))}
          </div>
        </div>
      ) : null}
    </div>
  );
}

export function SqlResultTable({ result }: { result: SqlQueryResult }) {
  // 数値の列（値が数値だけ・伏せた集計だけの列を含む）は見出しも値も右に揃える。
  const numeric = result.columns.map((_, c) => {
    const vals = result.rows.map((r) => r[c]);
    const present = vals.filter((v) => v !== null && v !== undefined);
    if (present.length === 0) return result.suppressed && vals.length > 0;
    return present.every((v) => typeof v === "number");
  });
  return (
    <div className="max-h-[70vh] overflow-auto rounded-xl border bg-card scrollbar-subtle" data-testid="sql-result">
      <table className="w-full border-collapse text-[13px]">
        <thead className="sticky top-0 z-10 bg-muted/90 backdrop-blur">
          <tr>
            <th className="w-10 border-b px-2 py-2 text-right text-[11px] font-medium text-muted-foreground" scope="col">
              #
            </th>
            {result.columns.map((c, i) => (
              <th
                key={`${c}-${i}`}
                scope="col"
                className={cn("whitespace-nowrap border-b px-3 py-2 text-[12px] font-semibold", numeric[i] ? "text-right" : "text-left")}
              >
                {c}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {result.rows.map((row, r) => (
            <tr key={r} className="border-b last:border-b-0 hover:bg-accent/40">
              <td className="px-2 py-1.5 text-right text-[11px] tabular-nums text-muted-foreground">{r + 1}</td>
              {row.map((v, c) => {
                // 伏せた集計は NULL で返る。値なしの集計も NULL なので、セルごとには理由を断定しない。
                const maybeHidden = v === null && numeric[c] && result.suppressed;
                return (
                  <td
                    key={c}
                    className={cn(
                      "max-w-[28rem] truncate px-3 py-1.5",
                      numeric[c] && "text-right tabular-nums",
                      v === null && "text-muted-foreground",
                    )}
                    title={
                      maybeHidden
                        ? "値が無いか、件数の少ないグループのため伏せています"
                        : typeof v === "number"
                          ? String(v)
                          : cellText(v)
                    }
                  >
                    {v === null ? "—" : cellText(v)}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
      {result.rows.length === 0 ? <p className="px-4 py-8 text-center text-sm text-muted-foreground">該当する行はありません。</p> : null}
    </div>
  );
}

/// 書き方の早見（チャットのツールの説明と同じ部分集合）。
export function SqlSyntaxHelp({ className }: { className?: string }) {
  return (
    <details className={cn("group rounded-xl border bg-card px-4 py-3 text-sm", className)} data-testid="sql-help">
      <summary className="flex cursor-pointer list-none items-center gap-2 font-medium [&::-webkit-details-marker]:hidden">
        <BookOpen className="size-4 text-muted-foreground" aria-hidden />
        使える SQL
        <ChevronDown className="ml-auto size-4 text-muted-foreground transition-transform duration-fast group-open:rotate-180" aria-hidden />
      </summary>
      <ul className="mt-2 list-disc space-y-1.5 pl-5 text-[13px] leading-6">
        <li>
          <code className="font-mono">SELECT 列, … FROM テーブル 別名</code>
          <span className="text-muted-foreground">（SELECT * は使えません。列は参照名）</span>
        </li>
        <li>
          結合はリンク列だけ: <code className="font-mono">FROM 案件 d JOIN 取引先 c ON d.customer = c._id</code>
          <span className="text-muted-foreground">（最大 3 個。絞り込みは WHERE に書く）</span>
        </li>
        <li>
          WHERE: <code className="font-mono">= &lt;&gt; &lt; &lt;= &gt; &gt;=</code>・AND / OR / NOT・IN・BETWEEN・LIKE・IS NULL
          <span className="text-muted-foreground">（日付は &apos;2026-10-01&apos;）</span>
        </li>
        <li>
          集計: COUNT(*)・SUM・AVG・MIN・MAX と GROUP BY・HAVING。
          <span className="text-muted-foreground">集計する行のテーブル（例: 案件）を FROM に書きます。件数の少ないグループは値を伏せます</span>
        </li>
        <li>
          ORDER BY は FROM のテーブルの列で並べます
          <span className="text-muted-foreground">（集計ではグループの列か集計）</span>
        </li>
        <li>
          LIMIT を書くとその件数で終わります。
          <span className="text-muted-foreground">省くと 50 件ずつ「さらに読み込む」で続きを読めます（行は 1 回 200 件まで）</span>
        </li>
        <li className="text-muted-foreground">DISTINCT・副問い合わせ・WITH・UNION・OFFSET・関数（UPPER など）・書き換えの文は使えません</li>
      </ul>
    </details>
  );
}
