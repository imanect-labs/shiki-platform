"use client";

/// SQL で問い合わせる画面（#978・data-platform §16.1）: 入力欄・結果の表・拒否の理由と位置・参照一覧。
///
/// SQL はサーバの SQL 前段面（`POST /data/sql`）が DataQuery へ変換して実行する（データベースへは届かない）。
/// 行述語・列のマスク・集計の抑制はサーバが閲覧者本人の権限で掛けるので、画面は返った表を出すだけにする。
/// 見えない名前と存在しない名前はサーバが同じ拒否にするので、画面も言い分けない。

import { ArrowUpRight, Loader2, Play, TableProperties } from "lucide-react";
import Link from "next/link";
import * as React from "react";

import { SqlReference, sqlIdent } from "@/components/data/sql-reference";
import { numberFmt, offsetOf, SqlErrorPanel, SqlResultTable, SqlSyntaxHelp } from "@/components/data/sql-result";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { DataApiError, listTables, runSql, type DataTable, type QueryRejection, type SqlQueryResult } from "@/lib/data-api";
import {
  readScoped,
  removeScoped,
  storageScopeGeneration,
  subscribeStorageScope,
  useStorageScopeReady,
  writeScoped,
} from "@/lib/user-storage";

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
function useSqlDraft(sql: string, setSql: React.Dispatch<React.SetStateAction<string>>): void {
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
}

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
  const editorRef = React.useRef<HTMLTextAreaElement>(null);
  /// 実行・続きの読み込みの世代（新しい実行が始まったら古い応答を捨てる）。
  const seq = React.useRef(0);
  const abortRef = React.useRef<AbortController | null>(null);

  useSqlDraft(sql, setSql);

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
  const example = React.useMemo(() => {
    const t = tables?.[0];
    if (!t) return "";
    // 例は値がそのまま読める列から選ぶ（リンク・ユーザー等は id が並ぶだけなので避ける）。
    const plain = new Set(["text", "number", "date", "date_time", "select"]);
    const cols = [...t.schema.fields]
      .sort((a, b) => (a.order ?? 0) - (b.order ?? 0))
      .filter((f) => plain.has(f.type))
      .slice(0, 3)
      .map((f) => sqlIdent(f.key));
    return `SELECT ${cols.join(", ") || "_id"} FROM ${sqlIdent(t.name)} LIMIT 50`;
  }, [tables]);

  /// [start, end) を `text` に置き換え、カーソルをその後ろへ置く。
  const replaceRange = React.useCallback((text: string, start: number, end: number) => {
    setSql((prev) => prev.slice(0, start) + text + prev.slice(end));
    requestAnimationFrame(() => {
      const el = editorRef.current;
      if (!el) return;
      el.focus();
      el.setSelectionRange(start + text.length, start + text.length);
    });
  }, []);

  const insert = React.useCallback(
    (text: string) => {
      const el = editorRef.current;
      replaceRange(text, el?.selectionStart ?? sql.length, el?.selectionEnd ?? sql.length);
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
    setLoadingMore(false);
    setError(null);
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
      const el = editorRef.current;
      if (loc && el) {
        const off = offsetOf(text, loc.line, loc.column);
        el.focus();
        el.setSelectionRange(off, nameEnd(text, off, isColumnRejection(rej)));
      }
    } finally {
      if (seq.current === id) setRunning(false);
    }
  }, [sql, withCount]);

  const loadMore = async () => {
    const cur = result;
    if (!cur?.data.next_cursor || loadingMore || running) return;
    const id = seq.current;
    const ac = new AbortController();
    abortRef.current = ac;
    setLoadingMore(true);
    setError(null);
    try {
      const data = await runSql({ sql: cur.sql, cursor: cur.data.next_cursor, with_count: false }, { signal: ac.signal });
      if (seq.current !== id) return;
      setResult((prev) =>
        prev && prev.sql === cur.sql
          ? { sql: prev.sql, data: { ...data, rows: [...prev.data.rows, ...data.rows], count: prev.data.count } }
          : prev,
      );
    } catch (e) {
      if (seq.current !== id || ac.signal.aborted) return;
      setError({ error: e, sql: cur.sql });
    } finally {
      if (seq.current === id) setLoadingMore(false);
    }
  };

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

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && !e.nativeEvent.isComposing) {
      e.preventDefault();
      void run();
    }
  };

  const data = result?.data;
  const tableOf = data ? tables?.find((t) => t.id === data.table_id) : undefined;
  // LIMIT を書いた問い合わせはその件数で終える（続きを読み込まない）。
  const hasMore = !!data?.next_cursor && data.query.limit == null;
  const isAggregate = (data?.query.aggregate?.length ?? 0) > 0;
  const countLabel = data
    ? data.count
      ? "exact" in data.count
        ? `${numberFmt.format(data.rows.length)} 行 / 全 ${numberFmt.format(data.count.exact)} 件`
        : `${numberFmt.format(data.rows.length)} 行 / 全 ${numberFmt.format(data.count.at_least)} 件以上`
      : hasMore
        ? `${numberFmt.format(data.rows.length)} 行を表示（続きあり）`
        : `${numberFmt.format(data.rows.length)} 行`
    : "";

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
            aria-invalid={rejection && error?.sql === sql ? true : undefined}
            className="min-h-28 resize-y font-mono text-[13px] leading-6 md:min-h-40 md:text-[13px]"
            data-testid="sql-editor"
          />
          <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
            <Button onClick={() => void run()} disabled={running || !sql.trim()} data-testid="sql-run">
              {running ? <Loader2 className="size-4 animate-spin" aria-hidden /> : <Play className="size-4" aria-hidden />}
              実行
            </Button>
            <span className="hidden text-xs text-muted-foreground sm:inline">
              <kbd className="rounded border bg-muted px-1 font-mono text-[11px]">⌘</kbd> /{" "}
              <kbd className="rounded border bg-muted px-1 font-mono text-[11px]">Ctrl</kbd> +{" "}
              <kbd className="rounded border bg-muted px-1 font-mono text-[11px]">Enter</kbd> でも実行
            </span>
            <Button
              variant="outline"
              size="sm"
              className="h-8 text-[13px]"
              onClick={() => setSql(example)}
              disabled={!example || !!sql.trim()}
            >
              例を入れる
            </Button>
            <label
              className="ml-auto flex items-center gap-2 text-[13px]"
              title="集計しない問い合わせの総件数（上限付き）。少し遅くなることがあります"
            >
              <Switch checked={withCount} onCheckedChange={setWithCount} aria-label="総件数も数える" />
              総件数も数える
            </label>
          </div>
        </div>

        {error ? (
          <SqlErrorPanel error={error.error} ranSql={error.sql} replaceable={replaceable} onCandidate={onCandidate} />
        ) : null}

        <section className="space-y-2" aria-label="結果" aria-busy={running || loadingMore}>
          <p className="sr-only" aria-live="polite">
            {running ? "実行中" : data ? countLabel : ""}
          </p>
          {data ? (
            <>
              <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5 text-[13px]">
                <span className="font-medium tabular-nums" data-testid="sql-result-count">
                  {countLabel}
                </span>
                {data.suppressed ? (
                  <Badge variant="warning" data-testid="sql-suppressed">
                    件数の少ないグループの値は伏せています（— で表示）
                  </Badge>
                ) : null}
                {isAggregate && withCount ? <Badge variant="muted">集計では総件数を数えません</Badge> : null}
                {data.sort_precision !== "exact" ? <Badge variant="muted">並び順は概算</Badge> : null}
                {data.shares_truncated ? <Badge variant="muted">個別共有の一部は対象外</Badge> : null}
                {tableOf ? (
                  <Link
                    href={`/data/${tableOf.id}`}
                    className="ml-auto inline-flex items-center gap-1 text-xs text-muted-foreground transition-colors duration-fast hover:text-foreground"
                  >
                    <TableProperties className="size-3.5" aria-hidden />
                    FROM のテーブル「{tableOf.name}」を開く
                    <ArrowUpRight className="size-3" aria-hidden />
                  </Link>
                ) : null}
              </div>
              <SqlResultTable result={data} />
              {hasMore ? (
                <div className="flex justify-center">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => void loadMore()}
                    disabled={loadingMore || running}
                    data-testid="sql-more"
                  >
                    {loadingMore ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
                    さらに読み込む
                  </Button>
                </div>
              ) : null}
            </>
          ) : null}
        </section>
      </div>

      <aside className="flex min-h-0 flex-col gap-4 lg:sticky lg:top-6 lg:max-h-[calc(100dvh-8rem)] lg:self-start">
        <SqlReference
          tables={tables}
          failed={tablesFailed}
          onRetry={loadTables}
          onInsert={insert}
          className="min-h-0 flex-1"
        />
        <SqlSyntaxHelp />
      </aside>
    </div>
  );
}
