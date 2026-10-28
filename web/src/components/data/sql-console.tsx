"use client";

/// SQL で問い合わせる画面（#978・data-platform §16.1）: 入力欄・結果の表・拒否の理由と位置・参照一覧。
///
/// SQL はサーバの SQL 前段面（`POST /data/sql`）が DataQuery へ変換して実行する（データベースへは届かない）。
/// 行述語・列のマスク・集計の抑制はサーバが閲覧者本人の権限で掛けるので、画面は返った表を出すだけにする。
/// 見えない名前と存在しない名前はサーバが同じ拒否にするので、画面も言い分けない。

import { ArrowUpRight, BookOpen, ChevronDown, CircleAlert, Loader2, Play, TableProperties } from "lucide-react";
import Link from "next/link";
import * as React from "react";

import { SqlReference, sqlTableName } from "@/components/data/sql-reference";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import {
  DataApiError,
  listTables,
  runSql,
  type DataTable,
  type QueryRejection,
  type SqlQueryResult,
} from "@/lib/data-api";
import { readScoped, writeScoped } from "@/lib/user-storage";
import { cn } from "@/lib/utils";

/// 書きかけの SQL の保存先（閲覧者のブラウザだけ・ユーザー単位。ログアウトで消える）。
const DRAFT_KEY = "shiki.sql-drafts.v1";

const REJECT_TITLE: Record<string, string> = {
  syntax_error: "SQL として読めません",
  unsupported: "使えない書き方です",
  unknown_name: "名前が見つかりません",
  invalid_join: "結合の書き方が違います",
  invalid_query: "問い合わせの組み立てが合いません",
  limit_exceeded: "上限を超えました",
};

const numberFmt = new Intl.NumberFormat("ja-JP", { maximumFractionDigits: 6 });

function cellText(v: unknown): string {
  if (v === null || v === undefined) return "";
  if (Array.isArray(v)) return v.map(cellText).join(", ");
  if (typeof v === "number") return numberFmt.format(v);
  if (typeof v === "boolean") return v ? "はい" : "いいえ";
  return String(v);
}

/// `line`・`column`（1 始まり）を文字列の位置へ。
function offsetOf(text: string, line: number, column: number): number {
  const lines = text.split("\n");
  let off = 0;
  for (let i = 0; i < Math.min(line - 1, lines.length); i++) off += lines[i].length + 1;
  return Math.min(off + Math.max(column - 1, 0), text.length);
}

/// 拒否の位置の行と印（等幅で `^` を置く）。
function LocationView({ sql, line, column }: { sql: string; line: number; column: number }) {
  const text = sql.split("\n")[line - 1];
  if (text === undefined) return null;
  return (
    <div className="overflow-x-auto rounded-md bg-muted/60 px-3 py-2 scrollbar-subtle" data-testid="sql-error-location">
      <pre className="font-mono text-[12.5px] leading-5 text-foreground">
        <span className="select-none text-muted-foreground">{String(line).padStart(2, " ")} │ </span>
        {text}
        {"\n"}
        <span className="select-none text-muted-foreground">{" ".repeat(String(line).padStart(2, " ").length)} │ </span>
        <span className="text-destructive">{" ".repeat(Math.max(column - 1, 0))}^</span>
      </pre>
    </div>
  );
}

function ErrorPanel({
  error,
  sql,
  onInsert,
}: {
  error: unknown;
  sql: string;
  onInsert: (text: string) => void;
}) {
  const rejection: QueryRejection | null = error instanceof DataApiError ? error.rejection : null;
  const title = rejection
    ? (REJECT_TITLE[rejection.code] ?? "受け付けられない問い合わせです")
    : error instanceof DataApiError && error.readTimeout
      ? "時間内に読み終わりませんでした"
      : "実行できませんでした";
  const message = rejection?.message ?? (error instanceof Error ? error.message : String(error));
  return (
    <div className="space-y-2.5 rounded-xl border border-destructive/40 bg-destructive/5 p-4" role="alert" data-testid="sql-error">
      <div className="flex items-start gap-2">
        <CircleAlert className="mt-0.5 size-4 shrink-0 text-destructive" aria-hidden />
        <div className="min-w-0 space-y-0.5">
          <p className="text-sm font-semibold">{title}</p>
          <p className="break-words text-sm text-foreground/90">{message}</p>
          {rejection?.location ? (
            <p className="text-xs text-muted-foreground">
              {rejection.location.line} 行 {rejection.location.column} 文字目
            </p>
          ) : null}
        </div>
      </div>
      {rejection?.location ? <LocationView sql={sql} line={rejection.location.line} column={rejection.location.column} /> : null}
      {rejection?.hint ? <p className="text-xs text-muted-foreground">手がかり: {rejection.hint}</p> : null}
      {rejection?.expected && rejection.expected.length > 0 ? (
        <div className="space-y-1">
          <p className="text-xs text-muted-foreground">使える候補（押すと入力欄に入ります）</p>
          <div className="flex flex-wrap gap-1.5" data-testid="sql-error-expected">
            {rejection.expected.slice(0, 40).map((e) => (
              <button
                key={e}
                type="button"
                onClick={() => onInsert(e)}
                className="rounded-md border bg-background px-2 py-0.5 font-mono text-[12px] transition-colors duration-fast hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                {e}
              </button>
            ))}
          </div>
        </div>
      ) : null}
    </div>
  );
}

function ResultTable({ result }: { result: SqlQueryResult }) {
  // 値が数値だけの列は見出しも右に揃える（値の桁と見出しの位置を合わせる）。
  const numeric = result.columns.map((_, c) => {
    const vals = result.rows.map((r) => r[c]).filter((v) => v !== null && v !== undefined);
    return vals.length > 0 && vals.every((v) => typeof v === "number");
  });
  return (
    <div className="overflow-auto rounded-xl border bg-card scrollbar-subtle" data-testid="sql-result">
      <table className="w-full border-collapse text-[13px]">
        <thead className="sticky top-0 z-10 bg-muted/80 backdrop-blur">
          <tr>
            <th className="w-10 border-b px-2 py-2 text-right text-[11px] font-medium text-muted-foreground" scope="col">
              #
            </th>
            {result.columns.map((c, i) => (
              <th
                key={`${c}-${i}`}
                scope="col"
                className={cn(
                  "whitespace-nowrap border-b px-3 py-2 font-mono text-[12px] font-medium",
                  numeric[i] ? "text-right" : "text-left",
                )}
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
              {row.map((v, c) => (
                <td
                  key={c}
                  className={cn(
                    "max-w-[28rem] truncate px-3 py-1.5",
                    typeof v === "number" && "text-right tabular-nums",
                    v === null && "text-muted-foreground",
                  )}
                  title={cellText(v)}
                >
                  {v === null ? "—" : cellText(v)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      {result.rows.length === 0 ? <p className="px-4 py-8 text-center text-sm text-muted-foreground">該当する行はありません。</p> : null}
    </div>
  );
}

/// 書き方の早見（チャットのツールの説明と同じ部分集合）。
function SyntaxHelp() {
  return (
    <details className="group rounded-xl border bg-card px-4 py-3 text-sm" data-testid="sql-help">
      <summary className="flex cursor-pointer list-none items-center gap-2 font-medium [&::-webkit-details-marker]:hidden">
        <BookOpen className="size-4 text-muted-foreground" aria-hidden />
        使える SQL
        <ChevronDown className="ml-auto size-4 text-muted-foreground transition-transform duration-fast group-open:rotate-180" aria-hidden />
      </summary>
      <ul className="mt-2 list-disc space-y-1 pl-5 text-[13px] leading-6 text-muted-foreground">
        <li>
          <code className="font-mono text-foreground">SELECT 列, … FROM テーブル [別名]</code>（<code className="font-mono">*</code>{" "}
          は使えません。列は参照名で書きます）
        </li>
        <li>
          結合はリンク列だけ: <code className="font-mono text-foreground">JOIN 先 別名 ON 元.リンク列 = 別名._id</code>（最大 3
          個。絞り込みは WHERE に）
        </li>
        <li>
          <code className="font-mono text-foreground">WHERE</code>: = &lt;&gt; &lt; &lt;= &gt; &gt;=・AND / OR / NOT・IN・BETWEEN・LIKE・IS
          NULL（日付は <code className="font-mono">&apos;2026-10-01&apos;</code>）
        </li>
        <li>
          集計: COUNT(*)・SUM・AVG・MIN・MAX と GROUP BY・HAVING。数える行のテーブルを FROM に置きます（件数の少ないグループは伏せます）
        </li>
        <li>ORDER BY は FROM のテーブルの列（集計ではグループの列か集計）。LIMIT は行 200・グループ 1000 まで</li>
        <li>DISTINCT・副問い合わせ・WITH・UNION・OFFSET・関数は使えません。データを書き換える文も使えません</li>
      </ul>
    </details>
  );
}

export function SqlConsole() {
  const [tables, setTables] = React.useState<DataTable[] | null>(null);
  const [sql, setSql] = React.useState("");
  const [withCount, setWithCount] = React.useState(false);
  const [running, setRunning] = React.useState(false);
  const [loadingMore, setLoadingMore] = React.useState(false);
  const [result, setResult] = React.useState<SqlQueryResult | null>(null);
  /// 結果を出した SQL（続きのページは同じ SQL と組で読む）。
  const [ranSql, setRanSql] = React.useState("");
  const [error, setError] = React.useState<unknown>(null);
  const editorRef = React.useRef<HTMLTextAreaElement>(null);
  const abortRef = React.useRef<AbortController | null>(null);

  React.useEffect(() => {
    listTables()
      .then((ts) => setTables([...ts].sort((a, b) => a.name.localeCompare(b.name, "ja"))))
      .catch(() => setTables([]));
    const saved = readScoped(DRAFT_KEY);
    if (saved) setSql(saved);
    return () => abortRef.current?.abort();
  }, []);

  // 書きかけを残す（失敗しても画面は動く）。
  React.useEffect(() => {
    const t = window.setTimeout(() => {
      try {
        writeScoped(DRAFT_KEY, sql);
      } catch {
        // Web Storage 不可でも問題ない。
      }
    }, 400);
    return () => window.clearTimeout(t);
  }, [sql]);

  const example = React.useMemo(() => {
    const t = tables?.[0];
    if (!t) return "";
    // 例は値がそのまま読める列から選ぶ（リンク・ユーザー等は id が並ぶだけなので避ける）。
    const plain = new Set(["text", "number", "date", "date_time", "select"]);
    const cols = [...t.schema.fields]
      .sort((a, b) => (a.order ?? 0) - (b.order ?? 0))
      .filter((f) => plain.has(f.type))
      .slice(0, 3)
      .map((f) => f.key);
    return `SELECT ${cols.join(", ") || "_id"} FROM ${sqlTableName(t.name)} LIMIT 50`;
  }, [tables]);

  const insert = React.useCallback((text: string) => {
    const el = editorRef.current;
    setSql((prev) => {
      const start = el?.selectionStart ?? prev.length;
      const end = el?.selectionEnd ?? prev.length;
      const next = prev.slice(0, start) + text + prev.slice(end);
      requestAnimationFrame(() => {
        if (!el) return;
        el.focus();
        el.setSelectionRange(start + text.length, start + text.length);
      });
      return next;
    });
  }, []);

  const run = React.useCallback(async () => {
    const text = sql.trim();
    if (!text || running) return;
    abortRef.current?.abort();
    const ac = new AbortController();
    abortRef.current = ac;
    setRunning(true);
    setError(null);
    try {
      const r = await runSql({ sql: text, with_count: withCount }, { signal: ac.signal });
      setResult(r);
      setRanSql(text);
    } catch (e) {
      if (ac.signal.aborted) return;
      setError(e);
      setResult(null);
      // 位置が分かる拒否は、入力欄のその位置へカーソルを置く（直してすぐ再実行できる）。
      const loc = e instanceof DataApiError ? e.rejection?.location : null;
      const el = editorRef.current;
      if (loc && el) {
        const off = offsetOf(sql, loc.line, loc.column);
        el.focus();
        el.setSelectionRange(off, off);
      }
    } finally {
      if (abortRef.current === ac) setRunning(false);
    }
  }, [sql, running, withCount]);

  const loadMore = async () => {
    if (!result?.next_cursor || loadingMore) return;
    setLoadingMore(true);
    try {
      const r = await runSql({ sql: ranSql, cursor: result.next_cursor, with_count: false });
      setResult({ ...r, rows: [...result.rows, ...r.rows], count: result.count });
    } catch (e) {
      setError(e);
    } finally {
      setLoadingMore(false);
    }
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && !e.nativeEvent.isComposing) {
      e.preventDefault();
      void run();
    }
  };

  const tableOf = result ? tables?.find((t) => t.id === result.table_id) : undefined;
  const count = result?.count;

  return (
    <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_17rem]">
      <div className="min-w-0 space-y-4">
        <div className="space-y-2">
          <label htmlFor="sql-editor" className="sr-only">
            SQL
          </label>
          <Textarea
            id="sql-editor"
            ref={editorRef}
            value={sql}
            onChange={(e) => setSql(e.target.value)}
            onKeyDown={onKeyDown}
            placeholder={example ? `例: ${example}` : "SELECT 列 FROM テーブル WHERE … ORDER BY … LIMIT 50"}
            spellCheck={false}
            autoCapitalize="off"
            autoCorrect="off"
            rows={7}
            aria-invalid={error instanceof DataApiError && error.rejection ? true : undefined}
            className="min-h-40 resize-y font-mono text-[13px] leading-6 md:text-[13px]"
            data-testid="sql-editor"
          />
          <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
            <Button onClick={() => void run()} disabled={running || !sql.trim()} data-testid="sql-run">
              {running ? <Loader2 className="size-4 animate-spin" aria-hidden /> : <Play className="size-4" aria-hidden />}
              実行
            </Button>
            <span className="hidden text-xs text-muted-foreground sm:inline">
              <kbd className="rounded border bg-muted px-1 font-mono text-[11px]">⌘</kbd> /{" "}
              <kbd className="rounded border bg-muted px-1 font-mono text-[11px]">Ctrl</kbd> +{" "}
              <kbd className="rounded border bg-muted px-1 font-mono text-[11px]">Enter</kbd> でも実行
            </span>
            <label className="ml-auto flex items-center gap-2 text-[13px]">
              <Switch checked={withCount} onCheckedChange={setWithCount} aria-label="件数も数える" />
              件数も数える
            </label>
            {example && !sql.trim() ? (
              <Button variant="ghost" size="sm" className="text-[13px]" onClick={() => setSql(example)}>
                例を入れる
              </Button>
            ) : null}
          </div>
        </div>

        {error ? <ErrorPanel error={error} sql={sql} onInsert={insert} /> : null}

        {result ? (
          <section className="space-y-2" aria-label="結果">
            <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5 text-[13px]">
              <span className="font-medium tabular-nums" data-testid="sql-result-count" aria-live="polite">
                {result.rows.length} 行
                {count
                  ? "exact" in count
                    ? `（全 ${numberFmt.format(count.exact)} 件）`
                    : `（全 ${numberFmt.format(count.at_least)} 件以上）`
                  : ""}
              </span>
              {result.suppressed ? (
                <Badge variant="warning" data-testid="sql-suppressed">
                  件数の少ないグループの値を伏せています
                </Badge>
              ) : null}
              {result.sort_precision !== "exact" ? <Badge variant="muted">並び順は概算</Badge> : null}
              {result.shares_truncated ? <Badge variant="muted">個別共有の一部は対象外</Badge> : null}
              {tableOf ? (
                <Link
                  href={`/data/${tableOf.id}`}
                  className="ml-auto inline-flex items-center gap-1 text-xs text-muted-foreground transition-colors duration-fast hover:text-foreground"
                >
                  <TableProperties className="size-3.5" aria-hidden />
                  「{tableOf.name}」を開く
                  <ArrowUpRight className="size-3" aria-hidden />
                </Link>
              ) : null}
            </div>
            <ResultTable result={result} />
            {result.next_cursor ? (
              <div className="flex justify-center">
                <Button variant="outline" size="sm" onClick={() => void loadMore()} disabled={loadingMore} data-testid="sql-more">
                  {loadingMore ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
                  さらに読み込む
                </Button>
              </div>
            ) : null}
          </section>
        ) : null}

        <SyntaxHelp />
      </div>

      <SqlReference
        tables={tables}
        onInsert={insert}
        className="lg:sticky lg:top-6 lg:max-h-[calc(100dvh-8rem)] lg:self-start"
      />
    </div>
  );
}
