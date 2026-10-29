"use client";

/// SQL で問い合わせる画面（#978・data-platform §16.1）: 入力欄・結果の表・拒否の理由と位置・参照一覧。
/// テーブルの画面と同じ作業画面の形（上のバーに実行・ツールバー・左に参照一覧・結果は同じグリッド）にする。
///
/// SQL はサーバの SQL 前段面（`POST /data/sql`）が DataQuery へ変換して実行する（データベースへは届かない）。
/// 行述語・列のマスク・集計の抑制はサーバが閲覧者本人の権限で掛けるので、画面は返った表を出すだけにする。
/// 見えない名前と存在しない名前はサーバが同じ拒否にするので、画面も言い分けない。

import { AlertTriangle, ArrowUpRight, BookOpen, EyeOff, FileCode2, Loader2, PanelLeft, Play, SquareTerminal, TableProperties } from "lucide-react";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import * as React from "react";

import type { SqlEditorHandle } from "@/components/data/sql-editor";
import { SqlReference, sqlIdent } from "@/components/data/sql-reference";
import { numberFmt, offsetOf, SqlErrorPanel, SqlSyntaxHelp } from "@/components/data/sql-result";
import { SqlResultGrid } from "@/components/data/sql-result-grid";
import { ChromePrimaryButton, usePageChrome } from "@/components/shell/page-header-context";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Switch } from "@/components/ui/switch";
import { useMediaQuery } from "@/hooks/use-media-query";
import { DataApiError, listTables, runSql, type DataTable, type QueryRejection, type SqlQueryResult } from "@/lib/data-api";
import {
  readScoped,
  removeScoped,
  storageScopeGeneration,
  subscribeStorageScope,
  useStorageScopeReady,
  writeScoped,
} from "@/lib/user-storage";
import { cn } from "@/lib/utils";

/// 書きかけの SQL の保存先（閲覧者のブラウザだけ・ユーザー単位。ログアウトで消える・isUserDataKey）。
const DRAFT_KEY = "shiki.sql-drafts.v1";
const DRAFT_SAVE_DELAY_MS = 400;
const DRAFT_MAX_CHARS = 64 * 1024;

function saveDraft(value: string, generation: number): void {
  if (generation !== storageScopeGeneration()) return;
  try {
    if (value.trim() && value.length <= DRAFT_MAX_CHARS) writeScoped(DRAFT_KEY, value);
    else removeScoped(DRAFT_KEY);
  } catch {
    // 容量超過・プライベートモードでも画面は動く。
  }
}

/// 書きかけの保存と復元。保存先がユーザー単位なので、ユーザーが確定してから読み、復元の後から書く
/// （復元前の空で保存値を消さない）。ログアウトで世代が進んだら入力欄から捨てる。
function useSqlDraft(sql: string, setSql: React.Dispatch<React.SetStateAction<string>>): boolean {
  const scopeReady = useStorageScopeReady();
  const [restored, setRestored] = React.useState(false);
  const generation = React.useRef(0);
  React.useEffect(
    () =>
      subscribeStorageScope(() => {
        if (storageScopeGeneration() === generation.current) return;
        setSql("");
        setRestored(false);
      }),
    [setSql],
  );
  React.useEffect(() => {
    if (!scopeReady || restored) return;
    generation.current = storageScopeGeneration();
    const saved = readScoped(DRAFT_KEY);
    if (saved) setSql((v) => v || saved);
    setRestored(true);
  }, [scopeReady, restored, setSql]);
  const latest = React.useRef(sql);
  React.useEffect(() => {
    latest.current = sql;
  }, [sql]);
  React.useEffect(() => {
    if (!restored) return;
    const g = generation.current;
    const t = window.setTimeout(() => saveDraft(sql, g), DRAFT_SAVE_DELAY_MS);
    return () => window.clearTimeout(t);
  }, [sql, restored]);
  React.useEffect(() => {
    if (!restored) return;
    const g = generation.current;
    const flush = () => saveDraft(latest.current, g);
    window.addEventListener("pagehide", flush);
    return () => {
      window.removeEventListener("pagehide", flush);
      flush();
    };
  }, [restored]);
  return restored;
}

/// テーブルの例の SQL（値がそのまま読める列を 3 つまで。リンク・ユーザー等は id が並ぶだけなので避ける）。
function exampleFor(t: DataTable): string {
  const plain = new Set(["text", "number", "date", "date_time", "select"]);
  const cols = [...t.schema.fields]
    .sort((a, b) => (a.order ?? 0) - (b.order ?? 0))
    .filter((f) => plain.has(f.type))
    .slice(0, 3)
    .map((f) => sqlIdent(f.key));
  return `SELECT ${cols.join(", ") || "_id"} FROM ${sqlIdent(t.name)} LIMIT 50`;
}

/// 入力欄（CodeMirror）は重いので、画面を先に出してから読み込む（ファイルの編集と同じ）。
const SqlEditor = React.lazy(() => import("@/components/data/sql-editor").then((m) => ({ default: m.SqlEditor })));

/// 結果の上の小さな印（テーブルの画面の「並び順は概算」と同じ形）。
const pill = "inline-flex items-center gap-1 whitespace-nowrap rounded-full border px-2 py-0.5 text-[11px] text-muted-foreground";

/// `off` から始まる名前の終わり。各部は引用符付きなら閉じるまで・それ以外は文字・数字・_ の並び。
/// 列の拒否は `別名.列` の先頭を指すので、`qualified` なら修飾名の全体を、テーブル（別名）の拒否なら
/// 先頭の 1 部だけを名前とする。
function nameEnd(text: string, off: number, qualified: boolean): number {
  let end = off;
  for (;;) {
    if (text[end] === '"') {
      const close = text.indexOf('"', end + 1);
      if (close < 0) return text.length;
      end = close + 1;
    } else {
      const start = end;
      while (end < text.length && /[\p{L}\p{N}_]/u.test(text[end])) end++;
      if (end === start) return end;
    }
    if (!qualified || text[end] !== "." || !/["\p{L}\p{N}_]/u.test(text[end + 1] ?? "")) return end;
    end++;
  }
}

/// 列の名前の拒否か（拒否文は「列 '…'」「テーブル '…'」で始まる。data の query/reject.rs）。
function isColumnRejection(r: QueryRejection | null | undefined): boolean {
  return r?.code === "unknown_name" && r.message.startsWith("列 ");
}

/// 候補を SQL に書ける形へ。列の候補は `別名.列` のことがあり、列の参照名には `.` を含まないので
/// 最後の `.` で分けて、それぞれを必要なら引用符で囲む。
function candidateSql(candidate: string, tableNames: Set<string>): string {
  if (tableNames.has(candidate)) return sqlIdent(candidate);
  const dot = candidate.lastIndexOf(".");
  return dot < 0 ? sqlIdent(candidate) : `${sqlIdent(candidate.slice(0, dot))}.${sqlIdent(candidate.slice(dot + 1))}`;
}

export function SqlConsole() {
  const params = useSearchParams();
  const fromId = params.get("table");
  const wide = useMediaQuery("(min-width: 1024px)");
  const [tables, setTables] = React.useState<DataTable[] | null>(null);
  const [tablesFailed, setTablesFailed] = React.useState(false);
  const [sql, setSql] = React.useState("");
  const [withCount, setWithCount] = React.useState(false);
  const [running, setRunning] = React.useState(false);
  const [loadingMore, setLoadingMore] = React.useState(false);
  /// 結果とそれを出した SQL（続きのページは同じ SQL と組で読む）。
  const [result, setResult] = React.useState<{ data: SqlQueryResult; sql: string } | null>(null);
  /// 誤りとそれを出した SQL（位置の表示・候補の置き換えはこの SQL で行う）。
  const [error, setError] = React.useState<{ error: unknown; sql: string } | null>(null);
  /// 続きのページの読み込みの失敗（理由の表示）。読み直しが成功するか、新しい実行で消す。
  const [moreError, setMoreError] = React.useState<unknown>(null);
  /// 失敗したあと自動の読み込みを止めているか（同じ位置を繰り返し読みに行かない）。「読み込み直す」で解く。
  const [moreBlocked, setMoreBlocked] = React.useState(false);
  /// 参照一覧の開閉（広い画面は開いて始める・押したら利用者の選択を優先する）。
  const [refChoice, setRefChoice] = React.useState<boolean | null>(null);
  const refOpen = refChoice ?? wide;
  /// テーブルの画面から開いたとき、書きかけを残したので例への置き換えを申し出る。
  const [offerFrom, setOfferFrom] = React.useState(false);
  const editorRef = React.useRef<SqlEditorHandle>(null);
  /// 実行・続きの読み込みの世代（新しい実行が始まったら古い応答を捨てる）。
  const seq = React.useRef(0);
  /// 続きの読み込み中か（同期で読む。描き直し前に表から続けて呼ばれても同じ位置を二重に読まない）。
  const loadingMoreRef = React.useRef(false);
  const abortRef = React.useRef<AbortController | null>(null);

  const restored = useSqlDraft(sql, setSql);

  const loadTables = React.useCallback(() => {
    setTablesFailed(false);
    setTables(null);
    listTables()
      .then((ts) => setTables([...ts].sort((a, b) => a.name.localeCompare(b.name, "ja"))))
      .catch(() => {
        setTables([]);
        setTablesFailed(true);
      });
  }, []);
  React.useEffect(() => {
    loadTables();
    return () => abortRef.current?.abort();
  }, [loadTables]);

  const tableNames = React.useMemo(() => new Set((tables ?? []).map((t) => t.name)), [tables]);
  const from = fromId ? tables?.find((t) => t.id === fromId) : undefined;
  const example = React.useMemo(() => {
    const t = from ?? tables?.[0];
    return t ? exampleFor(t) : "";
  }, [from, tables]);

  // テーブルの画面から開いたら、そのテーブルの例を入れる（書きかけがあれば残して、置き換えを申し出る）。
  const appliedFrom = React.useRef<string | null>(null);
  React.useEffect(() => {
    if (!from || !restored || appliedFrom.current === from.id) return;
    appliedFrom.current = from.id;
    if (sql.trim()) setOfferFrom(true);
    else setSql(exampleFor(from));
  }, [from, restored, sql]);

  /// [start, end) を `text` に置き換え、カーソルをその後ろへ置く。
  /// （入力欄は変わった範囲の後ろにカーソルを置く・sql-editor.tsx）
  const replaceRange = React.useCallback((text: string, start: number, end: number) => {
    setSql((prev) => prev.slice(0, start) + text + prev.slice(end));
    requestAnimationFrame(() => editorRef.current?.focus());
  }, []);

  const insert = React.useCallback(
    (text: string) => {
      const sel = editorRef.current?.selection() ?? { from: sql.length, to: sql.length };
      replaceRange(text, sel.from, sel.to);
    },
    [replaceRange, sql.length],
  );

  const run = React.useCallback(async () => {
    if (!sql.trim()) return;
    const id = ++seq.current;
    abortRef.current?.abort();
    const ac = new AbortController();
    abortRef.current = ac;
    // 送る SQL と位置の計算に使う SQL を同じにする（前後の空白も落とさない）。
    const text = sql;
    setRunning(true);
    loadingMoreRef.current = false;
    setLoadingMore(false);
    setError(null);
    setMoreError(null);
    setMoreBlocked(false);
    setOfferFrom(false);
    try {
      const data = await runSql({ sql: text, with_count: withCount }, { signal: ac.signal });
      if (seq.current !== id) return;
      setResult({ data, sql: text });
    } catch (e) {
      if (seq.current !== id || ac.signal.aborted) return;
      setError({ error: e, sql: text });
      setResult(null);
      // 位置が分かる拒否は、入力欄のその名前を選んでおく（直してすぐ再実行できる）。
      const rej = e instanceof DataApiError ? e.rejection : null;
      const loc = rej?.location;
      if (loc) {
        const off = offsetOf(text, loc.line, loc.column);
        editorRef.current?.select(off, nameEnd(text, off, isColumnRejection(rej)));
      }
    } finally {
      if (seq.current === id) setRunning(false);
    }
  }, [sql, withCount]);
  const runRef = React.useRef(run);
  React.useEffect(() => {
    runRef.current = run;
  }, [run]);

  const loadMore = React.useCallback(async () => {
    const cur = result;
    if (!cur?.data.next_cursor || loadingMoreRef.current || running) return;
    loadingMoreRef.current = true;
    const id = seq.current;
    const ac = new AbortController();
    abortRef.current = ac;
    setLoadingMore(true);
    try {
      const data = await runSql({ sql: cur.sql, cursor: cur.data.next_cursor, with_count: false }, { signal: ac.signal });
      if (seq.current !== id) return;
      setMoreError(null);
      // 伏せた値・個別共有の打ち切りは、どれかのページで起きていれば結果全体の印として残す。
      setResult((prev) =>
        prev && prev.sql === cur.sql
          ? {
              sql: prev.sql,
              data: {
                ...data,
                rows: [...prev.data.rows, ...data.rows],
                count: prev.data.count,
                suppressed: prev.data.suppressed || data.suppressed,
                shares_truncated: prev.data.shares_truncated || data.shares_truncated,
              },
            }
          : prev,
      );
    } catch (e) {
      if (seq.current !== id || ac.signal.aborted) return;
      setMoreError(e);
      setMoreBlocked(true);
    } finally {
      if (seq.current === id) {
        loadingMoreRef.current = false;
        setLoadingMore(false);
      }
    }
  }, [result, running]);

  const hasSql = sql.trim() !== "";
  usePageChrome(
    () => ({
      title: "SQL",
      back: from ? { href: `/data/${from.id}`, label: `「${from.name}」へ戻る` } : { href: "/data", label: "テーブル一覧へ" },
      primary: (
        <ChromePrimaryButton onClick={() => void runRef.current()} disabled={running || !hasSql} data-testid="sql-run">
          {running ? <Loader2 className="animate-spin" aria-hidden /> : <Play aria-hidden />}
          実行
        </ChromePrimaryButton>
      ),
    }),
    [from?.id, from?.name, running, hasSql],
  );

  const rejection = error?.error instanceof DataApiError ? error.error.rejection : null;
  // 入力欄が拒否された SQL のままなら、候補で誤った名前を置き換えられる。
  const replaceable = !!(rejection?.code === "unknown_name" && rejection.location && error?.sql === sql);
  const onCandidate = (candidate: string) => {
    const text = candidateSql(candidate, tableNames);
    if (replaceable && rejection?.location) {
      const off = offsetOf(sql, rejection.location.line, rejection.location.column);
      replaceRange(text, off, nameEnd(sql, off, isColumnRejection(rejection)));
    } else {
      insert(text);
    }
  };

  // 補完に使うテーブル → 列（閲覧者に読める列とシステム列）。
  const schema = React.useMemo(
    () =>
      Object.fromEntries(
        (tables ?? []).map((t) => [t.name, [...t.schema.fields.map((f) => f.key), "_id", "_owner", "_created_at", "_updated_at"]]),
      ),
    [tables],
  );

  const data = result?.data;
  const tableOf = data ? tables?.find((t) => t.id === data.table_id) : undefined;
  // LIMIT を書いた問い合わせはその件数で終える（続きを読み込まない）。
  const canMore = !!data?.next_cursor && data.query.limit == null;
  const hasMore = canMore && !moreBlocked;
  const isAggregate = (data?.query.aggregate?.length ?? 0) > 0;
  const countLabel = data
    ? data.count
      ? "exact" in data.count
        ? `${numberFmt.format(data.rows.length)} 行 / 全 ${numberFmt.format(data.count.exact)} 件`
        : `${numberFmt.format(data.rows.length)} 行 / 全 ${numberFmt.format(data.count.at_least)} 件以上`
      : canMore
        ? `${numberFmt.format(data.rows.length)} 行（続きあり）`
        : `${numberFmt.format(data.rows.length)} 行`
    : "";
  const invalid = !!rejection && error?.sql === sql;

  return (
    <div className="flex h-full min-h-0 flex-col gap-2 overflow-y-auto px-3 pb-3 pt-2 md:px-5 md:pt-4 lg:overflow-hidden">
      <h1 className="text-lg font-semibold leading-tight md:hidden">SQL</h1>
      <div className="flex flex-wrap items-center gap-x-0.5 gap-y-1.5">
        <Button
          variant="ghost"
          size="sm"
          className={cn("h-8 gap-1.5 px-2.5 text-[13px]", refOpen && "bg-accent")}
          aria-pressed={refOpen}
          onClick={() => setRefChoice(!refOpen)}
          data-testid="sql-reference-toggle"
        >
          <PanelLeft className="size-4" aria-hidden />
          テーブルと列
        </Button>
        <div className="mx-1 h-5 w-px bg-border" aria-hidden />
        <Popover>
          <PopoverTrigger asChild>
            <Button variant="ghost" size="sm" className="h-8 gap-1.5 px-2.5 text-[13px]" data-testid="sql-help-open">
              <BookOpen className="size-4" aria-hidden />
              使える SQL
            </Button>
          </PopoverTrigger>
          <PopoverContent align="start" className="w-[28rem] max-w-[calc(100vw-2rem)]">
            <SqlSyntaxHelp />
          </PopoverContent>
        </Popover>
        <Button
          variant="ghost"
          size="sm"
          className="h-8 gap-1.5 px-2.5 text-[13px]"
          onClick={() => setSql(example)}
          disabled={!example || hasSql}
          title={hasSql ? "入力欄が空のときに使えます" : undefined}
        >
          <FileCode2 className="size-4" aria-hidden />
          例を入れる
        </Button>
        <div className="ml-auto flex items-center gap-3 pl-2">
          <span className="hidden items-center gap-1 text-xs text-muted-foreground md:inline-flex">
            <kbd className="rounded border bg-muted px-1 font-mono text-[11px]">⌘</kbd>
            <kbd className="rounded border bg-muted px-1 font-mono text-[11px]">Enter</kbd>
            で実行
          </span>
          <label
            className="flex items-center gap-2 whitespace-nowrap text-[13px] text-muted-foreground"
            title="集計しない問い合わせの総件数（上限付き）。少し遅くなることがあります"
          >
            <Switch checked={withCount} onCheckedChange={setWithCount} aria-label="総件数も数える" />
            総件数も数える
          </label>
        </div>
      </div>

      <div className="flex min-h-0 flex-1 flex-col gap-3 lg:flex-row">
        {refOpen ? (
          <aside className="flex min-h-0 flex-col rounded-lg border bg-card p-2 max-lg:max-h-72 lg:w-64 lg:shrink-0">
            <SqlReference
              key={fromId ?? ""}
              tables={tables}
              failed={tablesFailed}
              onRetry={loadTables}
              onInsert={insert}
              focusId={fromId ?? undefined}
              className="min-h-0 flex-1"
            />
          </aside>
        ) : null}

        <div className="flex min-h-0 min-w-0 flex-1 flex-col gap-2">
          {offerFrom && from ? (
            <div className="flex flex-wrap items-center gap-2 rounded-lg border bg-muted/40 px-3 py-2 text-[13px]" role="status">
              <TableProperties className="size-4 shrink-0 text-muted-foreground" aria-hidden />
              <span className="min-w-0 flex-1">「{from.name}」から開きました。書きかけの SQL はそのまま残しています。</span>
              <Button
                size="sm"
                variant="outline"
                className="h-7 text-xs"
                onClick={() => {
                  setSql(exampleFor(from));
                  setOfferFrom(false);
                }}
              >
                「{from.name}」の例に置き換える
              </Button>
              <Button size="sm" variant="ghost" className="h-7 text-xs" onClick={() => setOfferFrom(false)}>
                閉じる
              </Button>
            </div>
          ) : null}

          <div
            className={cn(
              "h-40 max-h-[60vh] min-h-28 shrink-0 resize-y overflow-hidden rounded-lg border bg-card transition-[border-color,box-shadow] duration-fast focus-within:border-ring/60 focus-within:ring-2 focus-within:ring-ring/20 md:h-44",
              invalid && "border-destructive/60 focus-within:border-destructive/60 focus-within:ring-destructive/15",
            )}
          >
            <React.Suspense
              fallback={
                <pre className="h-full overflow-hidden whitespace-pre-wrap px-4 py-2 font-mono text-[13px] leading-[1.65] text-muted-foreground" aria-hidden>
                  {sql}
                </pre>
              }
            >
              <SqlEditor
                ref={editorRef}
                value={sql}
                onChange={setSql}
                onRun={() => void runRef.current()}
                schema={schema}
                placeholder={example ? `例: ${example}` : "SELECT 列 FROM テーブル WHERE … ORDER BY … LIMIT 50"}
                invalid={invalid}
              />
            </React.Suspense>
          </div>

          <section className="flex min-h-[18rem] flex-1 flex-col gap-1.5" aria-label="結果" aria-busy={running || loadingMore}>
            <p className="sr-only" aria-live="polite">
              {running ? "実行中" : data ? countLabel : ""}
            </p>
            {moreError && data ? (
              <div
                className="flex items-center gap-2 rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-[13px]"
                role="alert"
                data-testid="sql-more-error"
              >
                <AlertTriangle className="size-4 shrink-0 text-destructive" aria-hidden />
                <span className="min-w-0 flex-1">
                  続きを読み込めませんでした（{moreError instanceof Error ? moreError.message : String(moreError)}）
                </span>
                <Button
                  size="sm"
                  variant="outline"
                  className="h-7 text-xs"
                  onClick={() => setMoreBlocked(false)}
                  disabled={!moreBlocked || loadingMore}
                >
                  {loadingMore ? <Loader2 className="size-3.5 animate-spin" aria-hidden /> : null}
                  読み込み直す
                </Button>
              </div>
            ) : null}
            {data ? (
              <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                <span className="whitespace-nowrap text-xs tabular-nums text-muted-foreground" data-testid="sql-result-count">
                  {countLabel}
                </span>
                {data.suppressed ? (
                  <span
                    className={cn(pill, "text-foreground/80")}
                    style={{
                      borderColor: "color-mix(in oklab, var(--season-autumn) 40%, transparent)",
                      backgroundColor: "color-mix(in oklab, var(--season-autumn) 8%, transparent)",
                    }}
                    title="件数の少ないグループの値は「—」で出ます（値の無い集計も「—」です）"
                    data-testid="sql-suppressed"
                  >
                    <EyeOff className="size-3" aria-hidden />
                    件数の少ないグループの値は伏せています
                  </span>
                ) : null}
                {isAggregate && withCount ? <span className={pill}>集計では総件数を数えません</span> : null}
                {data.sort_precision !== "exact" ? (
                  <span className={pill}>
                    <AlertTriangle className="size-3" aria-hidden />
                    並び順は概算
                  </span>
                ) : null}
                {data.shares_truncated ? <span className={pill}>個別共有の一部は対象外</span> : null}
                {tableOf ? (
                  <Link
                    href={`/data/${tableOf.id}`}
                    className="ml-auto inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 text-xs text-muted-foreground transition-colors duration-fast hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  >
                    <TableProperties className="size-3.5" aria-hidden />
                    「{tableOf.name}」を開く
                    <ArrowUpRight className="size-3" aria-hidden />
                  </Link>
                ) : null}
              </div>
            ) : null}
            <div className={cn("min-h-0 flex-1 transition-opacity duration-fast", running && data && "opacity-60")}>
              {error && !data ? (
                <div className="h-full overflow-y-auto scrollbar-subtle">
                  <SqlErrorPanel error={error.error} ranSql={error.sql} replaceable={replaceable} onCandidate={onCandidate} />
                </div>
              ) : data ? (
                <SqlResultGrid result={data} hasMore={hasMore} loadingMore={loadingMore} onLoadMore={() => void loadMore()} />
              ) : (
                <div
                  className="flex h-full flex-col items-center justify-center gap-2 rounded-lg border border-dashed px-6 text-center text-sm text-muted-foreground"
                  data-testid="sql-empty"
                >
                  {running ? (
                    <>
                      <Loader2 className="size-5 animate-spin" aria-hidden />
                      実行中…
                    </>
                  ) : (
                    <>
                      <SquareTerminal className="size-6" aria-hidden />
                      <p>SQL を書いて実行すると、結果がここに出ます</p>
                      <p className="text-xs">
                        右上の「実行」か <kbd className="rounded border bg-muted px-1 font-mono text-[11px]">⌘</kbd> /{" "}
                        <kbd className="rounded border bg-muted px-1 font-mono text-[11px]">Ctrl</kbd> +{" "}
                        <kbd className="rounded border bg-muted px-1 font-mono text-[11px]">Enter</kbd>
                      </p>
                    </>
                  )}
                </div>
              )}
            </div>
          </section>
        </div>
      </div>
    </div>
  );
}
