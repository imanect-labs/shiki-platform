"use client";

/// ツール実行表示のギャラリー（デザイン確認・スクショ改善ループ用・issue #386）。
///
/// 認証・LLM 不要（middleware は /reference を除外）。実行中のローリング／完了後の
/// 折りたたみ／展開タイムライン／失敗表示／全 30 語彙のラベルを固定フィクスチャで並べる。
///
/// **なぜギャラリーが要るか**: 実行中（ローリング）の状態は stub LLM だとミリ秒で通過して
/// しまい、実機でスクショを撮れない。ここに固定状態で置くことで、ライト/ダーク両方の目視と
/// 回帰スクショが常に取れる。ラベル辞書（`lib/tool-display.ts`）の日本語が壊れていないかも
/// ここで一覧できる（30 語彙の網羅は `Record<ToolName, …>` が型で保証している）。

import * as React from "react";

import { ToolActivity, type ToolActivityItem } from "@/components/chat/tool-activity";

function Cell({
  id,
  title,
  children,
}: {
  id: string;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section data-testid={`tool-activity-${id}`} className="min-w-0">
      <h2 className="mb-1.5 text-[12px] font-medium text-muted-foreground">{title}</h2>
      {children}
    </section>
  );
}

/// 実行中（ローリング）: 5 件のうち直近 3 件だけが見える。
const RUNNING: ToolActivityItem[] = [
  {
    key: "r1",
    id: "r1",
    name: "web_search",
    running: false,
    ok: true,
    step: 0,
    input: { query: "2026 国内 SaaS 市場規模" },
    result: "web 検索結果 8 件",
  },
  {
    key: "r2",
    id: "r2",
    name: "doc_search",
    running: false,
    ok: true,
    step: 0,
    input: { query: "中期経営計画" },
    result: "社内文書 3 件",
  },
  {
    key: "r3",
    id: "r3",
    name: "web_fetch",
    running: true,
    step: 1,
    input: { url: "https://www.nikkei.com/article/DGXZQOUC1234567890" },
  },
  {
    key: "r4",
    id: "r4",
    name: "web_fetch",
    running: true,
    step: 1,
    input: { url: "https://www.itmedia.co.jp/news/articles/2607/29/news001.html" },
  },
  {
    key: "r5",
    id: "r5",
    name: "web_fetch",
    running: true,
    step: 1,
    input: { url: "https://www.meti.go.jp/policy/it_policy/statistics/outlook" },
  },
];

/// 完了（成功・失敗混在・並行あり）。展開すると step 境界でグループ化される。
const DONE: ToolActivityItem[] = [
  {
    key: "d1",
    id: "d1",
    name: "web_search",
    running: false,
    ok: true,
    step: 0,
    input: { query: "2026 国内 SaaS 市場規模" },
    result: "web 検索結果 8 件:\n[1] 国内 SaaS 市場は 2026 年に 1.6 兆円へ …",
  },
  {
    key: "d2",
    id: "d2",
    name: "web_fetch",
    running: false,
    ok: true,
    step: 1,
    input: { url: "https://www.nikkei.com/article/DGXZQOUC1234567890" },
    result: "HTTP 200\nContent-Type: text/html\n\n国内 SaaS 市場は 2026 年度に …",
  },
  {
    key: "d3",
    id: "d3",
    name: "web_fetch",
    running: false,
    ok: false,
    step: 1,
    input: { url: "https://example.invalid/ir/2026" },
    result: "取得に失敗しました: 名前解決に失敗",
  },
  {
    key: "d4",
    id: "d4",
    name: "fs_write",
    running: false,
    ok: true,
    step: 2,
    input: { name: "notes.md" },
    result: "notes.md を保存しました（3.2 KB）",
  },
  {
    key: "d5",
    id: "d5",
    name: "office.edit",
    running: false,
    ok: true,
    step: 3,
    input: { node_id: "00000000-0000-0000-0000-0000000000ab" },
    result: "1/1 件適用・新バージョン v12",
  },
];

/// 全 30 語彙のラベル確認（対象あり）。日本語が壊れていないかを一覧で見る。
const ALL_VOCAB: ToolActivityItem[] = [
  { name: "skill", input: { name: "deep-research" } },
  { name: "doc_search", input: { query: "就業規則" } },
  { name: "web_search", input: { query: "決算 2026" } },
  { name: "web_fetch", input: { url: "https://example.com/ir/2026/q1" } },
  { name: "code_interpreter", input: { code: "print(1)" } },
  { name: "fs_list", input: {} },
  { name: "fs_read", input: { name: "notes.md" } },
  { name: "grep", input: { pattern: "売上" } },
  { name: "fs_write", input: { name: "report.md" } },
  { name: "fs_edit", input: { name: "outline.md" } },
  { name: "fs_delete", input: { name: "tmp.txt" } },
  { name: "shell", input: { cmd: "ls -la" } },
  { name: "emit_ui", input: { spec: {} } },
  { name: "emit_workflow", input: {} },
  { name: "read_workflow", input: {} },
  { name: "document.read", input: {} },
  { name: "document.edit", input: {} },
  { name: "document.embed", input: {} },
  { name: "save_note", input: { name: "調査メモ" } },
  { name: "save_slide", input: { name: "提案資料" } },
  { name: "save_csv", input: { name: "売上明細" } },
  { name: "save_document", input: { name: "報告書" } },
  { name: "save_sheet", input: { name: "予算表" } },
  { name: "slide.read", input: {} },
  { name: "slide.edit", input: {} },
  { name: "office.edit", input: {} },
  { name: "office.live_edit", input: {} },
  { name: "csv.query", input: { sql: "SELECT category, count(*) FROM data GROUP BY category" } },
  { name: "csv.patch", input: {} },
  { name: "csv.write", input: { name: "集計結果" } },
  { name: "plan", input: {} },
].map((t, i) => ({ ...t, key: `v${i}`, id: `v${i}`, running: false, ok: true, step: i }));

/// ローリングの「動き」を確認するための再生デモ。
/// 実 LLM が無いとツールが 1 件ずつ増える様子を見られない（stub は一瞬で終わる）ため、
/// ここで一定間隔に追加して、新着が下から入り古い行が上へ抜けるのを目視する。
function RollingDemo() {
  const [count, setCount] = React.useState(1);
  const [playing, setPlaying] = React.useState(false);

  React.useEffect(() => {
    if (!playing) return;
    const id = window.setInterval(() => {
      setCount((c) => (c >= DEMO_STEPS.length ? 1 : c + 1));
    }, 1200);
    return () => window.clearInterval(id);
  }, [playing]);

  const items = DEMO_STEPS.slice(0, count).map((it, i) => ({
    ...it,
    running: i === count - 1,
    ok: i === count - 1 ? undefined : true,
  }));

  return (
    <div>
      <div className="mb-2 flex items-center gap-2">
        <button
          type="button"
          onClick={() => setPlaying((v) => !v)}
          className="rounded-md border border-border/60 px-2.5 py-1 text-[12px] transition-colors hover:bg-accent"
        >
          {playing ? "停止" : "再生"}
        </button>
        <span className="text-[12px] text-muted-foreground">
          {count} / {DEMO_STEPS.length} 件
        </span>
      </div>
      <ToolActivity items={items} streaming />
    </div>
  );
}

/// 再生デモで 1 件ずつ増えていくツール列（実際の deep research の進み方に寄せる）。
const DEMO_STEPS: ToolActivityItem[] = [
  { key: "s1",
    id: "s1", name: "web_search", running: false, step: 0, input: { query: "2026 国内 SaaS 市場規模" } },
  { key: "s2",
    id: "s2", name: "web_fetch", running: false, step: 1, input: { url: "https://www.nikkei.com/article/DGXZQOUC12" } },
  { key: "s3",
    id: "s3", name: "web_fetch", running: false, step: 1, input: { url: "https://www.itmedia.co.jp/news/articles/2607" } },
  { key: "s4",
    id: "s4", name: "doc_search", running: false, step: 2, input: { query: "中期経営計画 SaaS" } },
  { key: "s5",
    id: "s5", name: "fs_write", running: false, step: 3, input: { name: "notes.md" } },
  { key: "s6",
    id: "s6", name: "web_search", running: false, step: 4, input: { query: "SaaS 解約率 ベンチマーク" } },
  { key: "s7",
    id: "s7", name: "web_fetch", running: false, step: 5, input: { url: "https://www.meti.go.jp/policy/it_policy" } },
  { key: "s8",
    id: "s8", name: "fs_edit", running: false, step: 6, input: { name: "outline.md" } },
];

export default function ToolActivityGalleryPage() {
  return (
    <main className="mx-auto max-w-3xl space-y-8 p-6">
      <header>
        <h1 className="text-lg font-semibold tracking-tight">ツール実行表示</h1>
        <p className="mt-1 text-[13px] text-muted-foreground">
          チャットのツール実行可視化（issue #386）。実行中はフェーズ行＋直近 3
          件のローリング、完了後は 1 行要約に畳み、クリックで全件のタイムラインを開く。
        </p>
      </header>

      <Cell id="rolling-demo" title="ローリングの動き（再生で 1 件ずつ増える）">
        <RollingDemo />
      </Cell>

      <Cell id="running" title="実行中（フェーズ行＋ローリング 3 行・5 件中の直近 3 件）">
        <ToolActivity items={RUNNING} streaming />
      </Cell>

      <Cell id="running-with-plan" title="実行中（計画のサブタスクをフェーズ行に出す）">
        <ToolActivity items={RUNNING} streaming phaseOverride="市場規模と成長率を調べています" />
      </Cell>

      <Cell id="done" title="完了（折りたたみ 1 行要約。クリックで展開）">
        <ToolActivity items={DONE} />
      </Cell>

      <Cell id="vocab" title="全ツール語彙のラベル（展開して確認する）">
        <ToolActivity items={ALL_VOCAB} />
      </Cell>
    </main>
  );
}
