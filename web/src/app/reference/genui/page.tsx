"use client";

/// generative UI ギャラリー（デザイン確認・スクショ改善ループ用）。
///
/// 認証・LLM 不要（middleware は /reference を除外）。全 genui コンポーネントを
/// 固定フィクスチャで描画し、四季トークンとの整合をライト/ダーク両方で目視する。
/// SpecRenderer はサーバ検証済みスペックを前提にした静的マッピングだが、ここでは
/// 検証を通る形のフィクスチャを直接渡す（Provider 無し＝アクションは no-op）。

import * as React from "react";

import { SpecRenderer } from "@/components/genui/spec-renderer";

/// 1 コンポーネントのプレビュー枠。data-testid でスクショ選択できる。
function Cell({ id, title, spec }: { id: string; title: string; spec: unknown }) {
  return (
    <div data-testid={`gallery-${id}`} className="min-w-0">
      <div className="mb-1.5 text-[12px] font-medium text-muted-foreground">{title}</div>
      <SpecRenderer spec={spec} />
    </div>
  );
}

/// チャート 1 種のフィクスチャ（多系列・combo/stacked/xv も含む）。
function chart(kind: string, extra: Record<string, unknown> = {}): unknown {
  return {
    version: 1,
    root: {
      component: "chart",
      kind,
      title: `${kind} チャート`,
      data: [
        { x: "1月", y: 12, series: "実績", xv: 1 },
        { x: "2月", y: 19, series: "実績", xv: 2 },
        { x: "3月", y: 15, series: "実績", xv: 3 },
        { x: "4月", y: 24, series: "実績", xv: 4 },
        { x: "1月", y: 10, series: "目標", xv: 1 },
        { x: "2月", y: 16, series: "目標", xv: 2 },
        { x: "3月", y: 20, series: "目標", xv: 3 },
        { x: "4月", y: 22, series: "目標", xv: 4 },
      ],
      ...extra,
    },
  };
}

function stat(root: Record<string, unknown>): unknown {
  return { version: 1, root: { component: "stat", ...root } };
}

const CHARTS: { id: string; title: string; spec: unknown }[] = [
  { id: "bar", title: "棒（bar）", spec: chart("bar") },
  { id: "bar-stacked", title: "積み上げ棒", spec: chart("bar", { stacked: true }) },
  { id: "line", title: "折れ線（line）", spec: chart("line") },
  { id: "area", title: "面（area）", spec: chart("area") },
  { id: "area-stacked", title: "積み上げ面", spec: chart("area", { stacked: true }) },
  { id: "combo", title: "複合（combo）", spec: chart("combo", { line_series: ["目標"] }) },
  { id: "pie", title: "円（pie）", spec: chart("pie") },
  { id: "donut", title: "ドーナツ（donut）", spec: chart("donut") },
  { id: "scatter", title: "散布（scatter・数値 x）", spec: chart("scatter") },
  {
    id: "scatter-cat",
    title: "散布（scatter・カテゴリ x）",
    spec: {
      version: 1,
      root: {
        component: "chart",
        kind: "scatter",
        title: "カテゴリ別スコア",
        data: [
          { x: "A", y: 12, series: "実績" },
          { x: "B", y: 19, series: "実績" },
          { x: "C", y: 8, series: "実績" },
          { x: "A", y: 15, series: "目標" },
          { x: "B", y: 17, series: "目標" },
          { x: "C", y: 11, series: "目標" },
        ],
      },
    },
  },
  { id: "radar", title: "レーダー（radar）", spec: chart("radar") },
  { id: "radial", title: "放射状バー（radial_bar）", spec: chart("radial_bar") },
  { id: "funnel", title: "ファネル（funnel）", spec: chart("funnel") },
  { id: "treemap", title: "ツリーマップ（treemap）", spec: chart("treemap") },
];

const STATS: { id: string; title: string; spec: unknown }[] = [
  {
    id: "stat-up",
    title: "改善（正デルタ）",
    spec: stat({
      label: "今月の売上",
      value: "¥1.28M",
      delta: 12.4,
      delta_label: "前月比",
      trend: [8, 9.5, 9, 11, 10.5, 12.8],
      caption: "目標達成",
    }),
  },
  {
    id: "stat-down",
    title: "悪化（負デルタ）",
    spec: stat({
      label: "解約率",
      value: "3.2",
      unit: "%",
      delta: -1.8,
      delta_label: "前月比",
      trend: [5, 4.6, 4.1, 3.8, 3.5, 3.2],
    }),
  },
  {
    id: "stat-plain",
    title: "デルタ・トレンド無し",
    spec: stat({ label: "アクティブユーザー", value: "8,214", unit: "人" }),
  },
];

function node(root: Record<string, unknown>): unknown {
  return { version: 1, root };
}

const LAYOUT: { id: string; title: string; spec: unknown }[] = [
  {
    id: "callout",
    title: "callout（4 トーン）",
    spec: node({
      component: "container",
      children: [
        { component: "callout", tone: "info", title: "情報", text: "これは info トーンです。" },
        { component: "callout", tone: "success", title: "成功", text: "処理が完了しました。" },
        { component: "callout", tone: "warning", text: "警告: 残り容量が少なくなっています。" },
        { component: "callout", tone: "danger", title: "エラー", text: "接続に失敗しました。" },
      ],
    }),
  },
  {
    id: "accordion",
    title: "accordion",
    spec: node({
      component: "accordion",
      items: [
        {
          title: "配送について",
          open: true,
          children: [{ component: "text", text: "通常 2〜3 営業日でお届けします。" }],
        },
        {
          title: "返品ポリシー",
          children: [{ component: "text", text: "到着後 14 日以内なら返品できます。" }],
        },
      ],
    }),
  },
  {
    id: "tabs",
    title: "tabs",
    spec: node({
      component: "tabs",
      tabs: [
        { label: "概要", children: [{ component: "text", text: "プロジェクトの概要です。" }] },
        {
          label: "指標",
          children: [
            {
              component: "stat",
              label: "進捗",
              value: "72",
              unit: "%",
              delta: 4.1,
              delta_label: "前週比",
            },
          ],
        },
      ],
    }),
  },
  {
    id: "stepper",
    title: "stepper",
    spec: node({
      component: "stepper",
      steps: [
        { title: "要件定義", status: "done" },
        { title: "実装", status: "doing", description: "PR2 レイアウト基盤を作成中" },
        { title: "レビュー", status: "todo" },
        { title: "リリース", status: "todo" },
      ],
    }),
  },
  {
    id: "badge_list",
    title: "badge_list",
    spec: node({
      component: "badge_list",
      badges: [
        { label: "Rust", tone: "info" },
        { label: "安定", tone: "success" },
        { label: "レビュー中", tone: "warning" },
        { label: "破壊的変更", tone: "danger" },
        { label: "docs", tone: "neutral" },
      ],
    }),
  },
  {
    id: "key_value",
    title: "key_value",
    spec: node({
      component: "key_value",
      title: "注文詳細",
      items: [
        { key: "注文番号", value: "#A-10293" },
        { key: "ステータス", value: "発送済み" },
        { key: "合計", value: "¥12,800" },
      ],
    }),
  },
  {
    id: "code_block",
    title: "code_block",
    spec: node({
      component: "code_block",
      language: "typescript",
      code: 'const greet = (name: string) => `こんにちは、${name}さん`;\nconsole.log(greet("世界"));',
    }),
  },
];

export default function GenUiGalleryPage() {
  return (
    <main className="mx-auto max-w-6xl px-6 py-10">
      <h1 className="text-lg font-semibold text-foreground">generative UI ギャラリー</h1>
      <p className="mt-1 text-[13px] text-muted-foreground">
        全コンポーネントを固定フィクスチャで描画（デザイン確認・スクショ改善ループ用）。
      </p>

      <section className="mt-8">
        <h2 className="mb-3 text-[13px] font-semibold tracking-wide text-foreground/70">チャート</h2>
        <div className="grid grid-cols-1 gap-6 md:grid-cols-2 xl:grid-cols-3">
          {CHARTS.map((c) => (
            <Cell key={c.id} id={c.id} title={c.title} spec={c.spec} />
          ))}
        </div>
      </section>

      <section className="mt-10">
        <h2 className="mb-3 text-[13px] font-semibold tracking-wide text-foreground/70">
          KPI スタットタイル
        </h2>
        <div className="grid grid-cols-1 gap-6 sm:grid-cols-2 xl:grid-cols-3">
          {STATS.map((s) => (
            <Cell key={s.id} id={s.id} title={s.title} spec={s.spec} />
          ))}
        </div>
      </section>

      <section className="mt-10">
        <h2 className="mb-3 text-[13px] font-semibold tracking-wide text-foreground/70">
          レイアウト / コンテンツ
        </h2>
        <div className="grid grid-cols-1 gap-6 md:grid-cols-2 xl:grid-cols-3">
          {LAYOUT.map((l) => (
            <Cell key={l.id} id={l.id} title={l.title} spec={l.spec} />
          ))}
        </div>
      </section>
    </main>
  );
}
