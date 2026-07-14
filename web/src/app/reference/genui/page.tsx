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

/// 2 系列（実績 vs 目標）の月次データ（bar/line/area/combo/radar/scatter 用）。
const SERIES_DATA = [
  { x: "1月", y: 320, series: "実績", xv: 1 },
  { x: "2月", y: 410, series: "実績", xv: 2 },
  { x: "3月", y: 385, series: "実績", xv: 3 },
  { x: "4月", y: 512, series: "実績", xv: 4 },
  { x: "1月", y: 300, series: "目標", xv: 1 },
  { x: "2月", y: 360, series: "目標", xv: 2 },
  { x: "3月", y: 420, series: "目標", xv: 3 },
  { x: "4月", y: 480, series: "目標", xv: 4 },
];

/// 単一次元の構成比データ（pie/donut/treemap 用・流入チャネル）。
const CHANNEL_DATA = [
  { x: "オーガニック検索", y: 4200 },
  { x: "SNS", y: 2600 },
  { x: "リファラル", y: 1500 },
  { x: "広告", y: 1100 },
];

/// チャート 1 種のフィクスチャ。title は実運用を想定した具体名にする（開発用の仮題を避ける）。
function chart(
  kind: string,
  title: string,
  data: unknown = SERIES_DATA,
  extra: Record<string, unknown> = {},
): unknown {
  return { version: 1, root: { component: "chart", kind, title, data, ...extra } };
}

function stat(root: Record<string, unknown>): unknown {
  return { version: 1, root: { component: "stat", ...root } };
}

const CHARTS: { id: string; title: string; spec: unknown }[] = [
  { id: "bar", title: "棒（bar）", spec: chart("bar", "月次売上（実績 vs 目標・万円）") },
  {
    id: "bar-stacked",
    title: "積み上げ棒",
    spec: chart("bar", "四半期の内訳（積み上げ）", SERIES_DATA, { stacked: true }),
  },
  { id: "line", title: "折れ線（line）", spec: chart("line", "週次アクティブユーザーの推移") },
  { id: "area", title: "面（area）", spec: chart("area", "累計サインアップ数") },
  {
    id: "area-stacked",
    title: "積み上げ面",
    spec: chart("area", "プラン別 MRR の推移", SERIES_DATA, { stacked: true }),
  },
  {
    id: "combo",
    title: "複合（combo）",
    spec: chart("combo", "売上（棒）と目標ライン（線）", SERIES_DATA, { line_series: ["目標"] }),
  },
  { id: "pie", title: "円（pie）", spec: chart("pie", "流入チャネルの構成", CHANNEL_DATA) },
  { id: "donut", title: "ドーナツ（donut）", spec: chart("donut", "デバイス別セッション", CHANNEL_DATA) },
  {
    id: "scatter",
    title: "散布（scatter・数値 x）",
    spec: chart(
      "scatter",
      "広告費とコンバージョンの相関",
      [
        { x: "キャンペーン1", xv: 15, y: 48, series: "検索広告" },
        { x: "キャンペーン2", xv: 32, y: 95, series: "検索広告" },
        { x: "キャンペーン3", xv: 48, y: 128, series: "検索広告" },
        { x: "キャンペーン4", xv: 22, y: 60, series: "SNS広告" },
        { x: "キャンペーン5", xv: 40, y: 84, series: "SNS広告" },
        { x: "キャンペーン6", xv: 62, y: 138, series: "SNS広告" },
      ],
      { x_label: "広告費（万円）", y_label: "CV数" },
    ),
  },
  {
    id: "scatter-cat",
    title: "散布（scatter・カテゴリ x）",
    spec: chart("scatter", "拠点別スコア（実績 vs 目標）", [
      { x: "東京", y: 82, series: "実績" },
      { x: "大阪", y: 74, series: "実績" },
      { x: "福岡", y: 68, series: "実績" },
      { x: "東京", y: 78, series: "目標" },
      { x: "大阪", y: 80, series: "目標" },
      { x: "福岡", y: 72, series: "目標" },
    ]),
  },
  {
    id: "radar",
    title: "レーダー（radar）",
    spec: chart("radar", "スキル評価（現状 vs 目標）", [
      { x: "技術力", y: 82, series: "現状" },
      { x: "コミュ力", y: 70, series: "現状" },
      { x: "設計", y: 65, series: "現状" },
      { x: "運用", y: 78, series: "現状" },
      { x: "スピード", y: 60, series: "現状" },
      { x: "技術力", y: 90, series: "目標" },
      { x: "コミュ力", y: 80, series: "目標" },
      { x: "設計", y: 85, series: "目標" },
      { x: "運用", y: 82, series: "目標" },
      { x: "スピード", y: 75, series: "目標" },
    ]),
  },
  {
    id: "radial",
    title: "放射状バー（radial_bar）",
    spec: chart("radial_bar", "チャネル別 目標達成率（%）", [
      { x: "SNS", y: 82 },
      { x: "オーガニック検索", y: 68 },
      { x: "リファラル", y: 91 },
      { x: "広告", y: 54 },
    ]),
  },
  { id: "funnel", title: "ファネル（funnel）", spec: chart("funnel", "購入ファネル", [
    { x: "訪問", y: 12000 },
    { x: "カート", y: 5200 },
    { x: "決済開始", y: 2400 },
    { x: "購入完了", y: 1500 },
  ]) },
  { id: "treemap", title: "ツリーマップ（treemap）", spec: chart("treemap", "カテゴリ別売上", CHANNEL_DATA) },
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

const RICH_FORM: unknown = {
  version: 1,
  actions: [{ type: "handler", id: "submit", handler: "chat.submit" }],
  root: {
    component: "form",
    id: "survey",
    title: "アンケート",
    submit: { action: "submit" },
    submit_label: "送信",
    fields: [
      { component: "text_input", id: "name", label: "お名前", placeholder: "山田太郎" },
      {
        component: "select",
        id: "plan",
        label: "プラン",
        options: [
          { value: "free", label: "無料" },
          { value: "pro", label: "Pro" },
        ],
        allow_other: true,
      },
      {
        component: "radio",
        id: "freq",
        label: "利用頻度",
        options: [
          { value: "daily", label: "毎日" },
          { value: "weekly", label: "毎週" },
        ],
        default: "daily",
      },
      {
        component: "checkbox",
        id: "features",
        label: "使う機能",
        options: [
          { value: "chat", label: "チャット" },
          { value: "rag", label: "RAG" },
          { value: "wf", label: "ワークフロー" },
        ],
        default: ["chat"],
        allow_other: true,
      },
      { component: "date", id: "start", label: "開始日" },
      { component: "date", id: "period", label: "利用期間", range: true },
      { component: "slider", id: "budget", label: "予算（万円）", min: 0, max: 100, step: 5, default: 30 },
      { component: "rating", id: "nps", label: "満足度", max: 5, default: 4 },
    ],
  },
};

const QUESTION_CARD: unknown = {
  version: 1,
  actions: [{ type: "handler", id: "answer", handler: "chat.submit" }],
  root: {
    component: "question_card",
    id: "trip",
    title: "旅行プランの確認",
    intro: "ぴったりの旅程を提案するために、いくつか教えてください。",
    submit: { action: "answer" },
    submit_label: "回答する",
    questions: [
      {
        id: "purpose",
        header: "目的",
        question: "今回の旅行の主な目的は何ですか？",
        options: [
          { label: "観光・レジャー", description: "名所や自然、グルメなど旅先を楽しむのが中心" },
          { label: "出張・ビジネス", description: "会議や商談が主目的。移動効率と宿の作業環境を重視" },
          { label: "帰省・イベント", description: "家族の集まりや結婚式・ライブなど特定の予定に合わせる" },
        ],
        allow_other: true,
      },
      {
        id: "pace",
        header: "ペース",
        question: "旅のペースはどれくらいが好みですか？",
        options: [
          { label: "ゆったり", description: "1 日 1〜2 か所。休憩やカフェの時間をしっかり取る" },
          { label: "しっかり", description: "主要スポットを効率よく巡る、バランス型" },
          { label: "詰め込み", description: "朝から晩まで、行けるところは全部回りたい" },
        ],
      },
      {
        id: "interests",
        header: "興味",
        question: "特に興味があるものはどれですか？（複数選択できます）",
        options: [
          { label: "グルメ", description: "地元の名物や話題の店を巡りたい" },
          { label: "自然・絶景", description: "山・海・公園など景色を楽しみたい" },
          { label: "歴史・文化", description: "寺社・城・博物館など" },
          { label: "ショッピング", description: "買い物や土産選びを楽しみたい" },
        ],
        multi_select: true,
        allow_other: true,
      },
      {
        id: "notes",
        question: "その他、希望や制約があれば自由にお書きください。",
        placeholder: "例: 子ども連れ／車椅子で移動／予算は 1 人 5 万円まで など",
      },
    ],
  },
};

/// 地図（マーカー＋徒歩ルート・座標のみ／タイルはサーバ設定・PR5）。東京の半日さんぽ。
const MAP_CARD: unknown = {
  version: 1,
  root: {
    component: "map",
    title: "東京 半日さんぽ（徒歩ルート）",
    center: { lat: 35.665, lng: 139.752 },
    zoom: 13,
    markers: [
      { lat: 35.6812, lng: 139.7671, label: "東京駅", description: "出発 10:00", kind: "start" },
      { lat: 35.6586, lng: 139.7454, label: "東京タワー", description: "展望 11:00", kind: "sight" },
      { lat: 35.6604, lng: 139.7292, label: "六本木で昼食", description: "12:30", kind: "food" },
      { lat: 35.6852, lng: 139.7528, label: "皇居東御苑", description: "散策 14:30", kind: "sight" },
      { lat: 35.6749, lng: 139.763, label: "有楽町のホテル", description: "16:00", kind: "lodging" },
    ],
    // route.waypoints は徒歩ルータで実際の街路に沿わせた密なポリライン（AI が経路ツールで
    // 出す想定の形）。クライアントは外部へ問い合わせず、この経路をそのまま描く。
    route: {
      mode: "walking",
      waypoints: [
        { lat: 35.6812, lng: 139.76711 }, { lat: 35.68048, lng: 139.7659 }, { lat: 35.67941, lng: 139.7639 }, { lat: 35.67862, lng: 139.76293 },
        { lat: 35.67833, lng: 139.76216 }, { lat: 35.67545, lng: 139.75974 }, { lat: 35.67409, lng: 139.75835 }, { lat: 35.6697, lng: 139.75528 },
        { lat: 35.66782, lng: 139.75447 }, { lat: 35.6647, lng: 139.75302 }, { lat: 35.66301, lng: 139.75222 }, { lat: 35.66104, lng: 139.75125 },
        { lat: 35.6599, lng: 139.74957 }, { lat: 35.65971, lng: 139.74723 }, { lat: 35.65917, lng: 139.74533 }, { lat: 35.65952, lng: 139.74313 },
        { lat: 35.66007, lng: 139.74055 }, { lat: 35.66139, lng: 139.73704 }, { lat: 35.662, lng: 139.73496 }, { lat: 35.66228, lng: 139.73271 },
        { lat: 35.66113, lng: 139.73045 }, { lat: 35.66068, lng: 139.72955 }, { lat: 35.66075, lng: 139.72969 }, { lat: 35.6617, lng: 139.73023 },
        { lat: 35.6619, lng: 139.73046 }, { lat: 35.66337, lng: 139.73199 }, { lat: 35.66431, lng: 139.73404 }, { lat: 35.6685, lng: 139.74013 },
        { lat: 35.67054, lng: 139.74219 }, { lat: 35.6719, lng: 139.74379 }, { lat: 35.67372, lng: 139.74743 }, { lat: 35.67667, lng: 139.74926 },
        { lat: 35.67737, lng: 139.7505 }, { lat: 35.677, lng: 139.75579 }, { lat: 35.68452, lng: 139.76027 }, { lat: 35.68588, lng: 139.76037 },
        { lat: 35.68607, lng: 139.75804 }, { lat: 35.68518, lng: 139.75687 }, { lat: 35.68573, lng: 139.75517 }, { lat: 35.68616, lng: 139.75473 },
        { lat: 35.68574, lng: 139.75536 }, { lat: 35.68501, lng: 139.75729 }, { lat: 35.68623, lng: 139.75827 }, { lat: 35.68572, lng: 139.761 },
        { lat: 35.68466, lng: 139.76278 }, { lat: 35.68221, lng: 139.76346 }, { lat: 35.67942, lng: 139.76275 }, { lat: 35.67587, lng: 139.76286 },
        { lat: 35.67488, lng: 139.76303 },
      ],
    },
  },
};

/// ドメインカード（PR6・RAG/旅行/意思決定ユース）。
const DOMAIN: { id: string; title: string; spec: unknown }[] = [
  {
    id: "source_card",
    title: "source_card（RAG 引用元）",
    spec: node({
      component: "source_card",
      title: "参照した資料",
      sources: [
        {
          title: "設計ドキュメント — 二段 authz",
          snippet: "RAG/構造化データは pre-filter と post-filter の両方を通す。実効権限 = スコープ ∩ ReBAC。",
          url: "https://example.com/docs/design",
          score: 0.94,
          label: "PDF",
        },
        {
          title: "オンボーディングガイド",
          snippet: "AuthContext { principal, org, tenant_id } を通して全データアクセスを行う。",
          url: "https://example.com/guide",
          score: 0.81,
          label: "Web",
        },
      ],
    }),
  },
  {
    id: "itinerary",
    title: "itinerary（旅程）",
    spec: node({
      component: "itinerary",
      title: "東京 日帰りプラン",
      days: [
        {
          label: "1 日目",
          date: "7/13(日)",
          items: [
            { time: "10:00", title: "東京駅 集合", location: "丸の内北口", kind: "travel" },
            { time: "11:00", title: "東京タワー", description: "展望デッキから都心を一望", location: "芝公園", kind: "sight" },
            { time: "12:30", title: "六本木でランチ", location: "六本木ヒルズ", kind: "food" },
            { time: "16:00", title: "有楽町のホテルにチェックイン", kind: "lodging" },
          ],
        },
      ],
    }),
  },
  {
    id: "weather",
    title: "weather（天気）",
    spec: node({
      component: "weather",
      title: "週末の予報",
      location: "東京",
      days: [
        { label: "今日", condition: "sunny", high: 31, low: 24, precipitation: 10 },
        { label: "明日", condition: "partly_cloudy", high: 29, low: 23, precipitation: 30 },
        { label: "水", condition: "rain", high: 26, low: 22, precipitation: 80 },
        { label: "木", condition: "cloudy", high: 28, low: 23, precipitation: 40 },
      ],
    }),
  },
  {
    id: "comparison",
    title: "comparison（比較）",
    spec: node({
      component: "comparison",
      title: "プラン比較",
      columns: ["Free", "Pro", "Enterprise"],
      highlight: 1,
      rows: [
        { label: "月額", values: ["¥0", "¥1,480", "要問合せ"] },
        { label: "ストレージ", values: ["1 GB", "100 GB", "無制限"] },
        { label: "サポート", values: ["コミュニティ", "メール", "専任担当"] },
        { label: "SLA", values: ["—", "99.9%", "99.99%"] },
      ],
    }),
  },
  {
    id: "timeline",
    title: "timeline（時系列）",
    spec: node({
      component: "timeline",
      title: "リリース履歴",
      events: [
        { time: "2026-05", title: "Phase 5 自律エージェント", description: "Durable Workspace と承認ゲート", tone: "success" },
        { time: "2026-06", title: "Phase 6 generative UI", description: "信頼カタログと emit_ui", tone: "info" },
        { time: "2026-07", title: "genui 拡充（進行中）", description: "地図・ドメインカード", tone: "warning" },
      ],
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

      <section className="mt-10">
        <h2 className="mb-3 text-[13px] font-semibold tracking-wide text-foreground/70">
          リッチ入力フォーム
        </h2>
        <div className="max-w-md">
          <Cell id="rich-form" title="全フィールド" spec={RICH_FORM} />
        </div>
      </section>

      <section className="mt-10">
        <h2 className="mb-3 text-[13px] font-semibold tracking-wide text-foreground/70">
          質問カード（AI からの問いかけ）
        </h2>
        <div className="max-w-md">
          <Cell id="question-card" title="複数質問＋自由記述" spec={QUESTION_CARD} />
        </div>
      </section>

      <section className="mt-10">
        <h2 className="mb-3 text-[13px] font-semibold tracking-wide text-foreground/70">
          地図（ルート＋マーカー）
        </h2>
        <div className="max-w-xl">
          <Cell id="map" title="旅程の徒歩ルート" spec={MAP_CARD} />
        </div>
      </section>

      <section className="mt-10">
        <h2 className="mb-3 text-[13px] font-semibold tracking-wide text-foreground/70">
          ドメインカード（RAG / 旅行 / 意思決定）
        </h2>
        <div className="grid grid-cols-1 gap-6 md:grid-cols-2">
          {DOMAIN.map((d) => (
            <Cell key={d.id} id={d.id} title={d.title} spec={d.spec} />
          ))}
        </div>
      </section>
    </main>
  );
}
