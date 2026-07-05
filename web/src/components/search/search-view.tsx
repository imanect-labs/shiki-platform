"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useRouter, useSearchParams } from "next/navigation";
import { FileSearch } from "lucide-react";

import { EmptyState } from "@/components/ui/empty-state";
import { Skeleton } from "@/components/ui/skeleton";
import {
  searchDocuments,
  SearchApiError,
  type SearchMode,
  type SearchResponse,
} from "@/lib/search";

import { DebugPanel } from "./debug-panel";
import { ResultCard } from "./result-card";
import { SearchInput } from "./search-input";

/// 文書検索の全体オーケストレーション。
/// クエリは URL（`?q=`）と同期し、検索結果 URL を共有できるようにする。
export function SearchView() {
  const router = useRouter();
  const params = useSearchParams();
  const initialQuery = params.get("q") ?? "";

  const [query, setQuery] = useState(initialQuery);
  const [mode, setMode] = useState<SearchMode>("hybrid");
  const [debug, setDebug] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [response, setResponse] = useState<SearchResponse | null>(null);
  /// 直近に実行したクエリ（結果ヘッダとハイライトに使う。入力途中の値と区別する）。
  const [executedQuery, setExecutedQuery] = useState("");
  const requestSeq = useRef(0);

  const runSearch = useCallback(
    async (q: string, m: SearchMode, withDebug: boolean) => {
      const trimmed = q.trim();
      if (!trimmed) return;
      const seq = ++requestSeq.current;
      setLoading(true);
      setError(null);
      try {
        const res = await searchDocuments({
          query: trimmed,
          mode: m,
          debug: withDebug,
        });
        // 古いリクエストの応答で新しい結果を上書きしない。
        if (seq !== requestSeq.current) return;
        setResponse(res);
        setExecutedQuery(trimmed);
        router.replace(`/search?q=${encodeURIComponent(trimmed)}`, { scroll: false });
      } catch (e) {
        if (seq !== requestSeq.current) return;
        setResponse(null);
        setError(e instanceof SearchApiError ? e.message : "検索に失敗しました");
      } finally {
        if (seq === requestSeq.current) setLoading(false);
      }
    },
    [router],
  );

  // URL 直リンク（?q=）で開いたら自動検索する。
  const bootstrapped = useRef(false);
  useEffect(() => {
    if (bootstrapped.current) return;
    bootstrapped.current = true;
    if (initialQuery.trim()) {
      void runSearch(initialQuery, "hybrid", false);
    }
  }, [initialQuery, runSearch]);

  return (
    <div className="flex flex-col gap-6">
      <SearchInput
        query={query}
        onQueryChange={setQuery}
        mode={mode}
        onModeChange={(m) => {
          setMode(m);
          if (executedQuery) void runSearch(executedQuery, m, debug);
        }}
        debug={debug}
        onDebugChange={(d) => {
          setDebug(d);
          if (executedQuery) void runSearch(executedQuery, mode, d);
        }}
        onSubmit={() => void runSearch(query, mode, debug)}
        loading={loading}
      />

      {error ? (
        <div
          role="alert"
          className="rounded-lg border border-destructive/30 bg-destructive/5 px-4 py-3 text-sm text-destructive"
        >
          {error}
        </div>
      ) : null}

      {loading ? (
        <div className="flex flex-col gap-3" aria-label="検索中">
          <Skeleton className="h-28 w-full" />
          <Skeleton className="h-28 w-full" />
          <Skeleton className="h-28 w-full" />
        </div>
      ) : null}

      {!loading && response ? (
        <>
          {response.debug ? <DebugPanel debug={response.debug} /> : null}
          {response.results.length === 0 ? (
            <EmptyState
              icon={FileSearch}
              title="一致する文書が見つかりませんでした"
              description="言い換えて検索するか、対象の文書が共有されているか確認してください。閲覧権限のない文書は検索対象になりません。"
            />
          ) : (
            <div className="flex flex-col gap-3" aria-label="検索結果">
              <p className="text-xs text-muted-foreground">
                「{executedQuery}」の検索結果 {response.results.length} 件
                （閲覧可能な文書のみ）
              </p>
              {response.results.map((result) => (
                <ResultCard
                  key={result.chunk_id}
                  result={result}
                  query={executedQuery}
                  showScore={debug}
                />
              ))}
            </div>
          )}
        </>
      ) : null}

      {!loading && !response && !error ? (
        <EmptyState
          icon={FileSearch}
          title="ドライブの文書を検索"
          description="自然文でもキーワードでも検索できます。結果は引用元のチャンク単位で表示され、あなたが閲覧できる文書だけが対象です。"
        />
      ) : null}
    </div>
  );
}
