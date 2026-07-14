"use client";

/// ドメインカード（PR6・RAG/旅行/意思決定ユースの表示専用コンポーネント）。
///
/// 配色/枠/区切りはアプリのデザイン言語に準拠する（選択/強調は塗り bg-accent、枠は柔らかく
/// border-border/60、背景は半透明 bg-card/40、区切りは破線グラデ shiki-dash、差し色は season
/// トークン）。表示専用で任意 HTML/コード実行の口は持たない（出典 URL は https のみ・検証済み）。

import * as React from "react";

import {
  BedDouble,
  Camera,
  Circle,
  Clock,
  Cloud,
  CloudFog,
  CloudLightning,
  CloudRain,
  CloudSun,
  ExternalLink,
  GitCompare,
  MapPin,
  Navigation,
  Quote,
  Snowflake,
  Sun,
  Utensils,
} from "lucide-react";

import type {
  ComparisonProps,
  ItineraryKind,
  ItineraryProps,
  SourceCardProps,
  TimelineProps,
  WeatherCondition,
  WeatherProps,
} from "@/generated/gui-spec";
import { currentSeasonIndex, seasonAccentStyle } from "@/lib/season";
import { cn } from "@/lib/utils";

/// 季節アクセントを注入したカード枠（各カード共通の外装）。
function DomainCard({
  icon,
  title,
  subtitle,
  children,
  testId,
}: {
  icon: React.ReactNode;
  title: string | null;
  subtitle?: string | null;
  children: React.ReactNode;
  testId: string;
}) {
  return (
    <section
      data-testid={testId}
      style={seasonAccentStyle(currentSeasonIndex())}
      className="min-w-0 overflow-hidden rounded-xl border border-border/60 bg-card/40"
    >
      {title !== null ? (
        <header className="flex items-center gap-2 px-3.5 py-2.5 shiki-dash-bottom">
          <span
            className="grid size-7 shrink-0 place-items-center rounded-lg"
            style={{
              backgroundColor: "color-mix(in oklab, var(--season) 16%, transparent)",
              color: "var(--season)",
            }}
            aria-hidden
          >
            {icon}
          </span>
          <div className="min-w-0">
            <h3 className="truncate text-sm font-semibold tracking-tight text-foreground">
              {title}
            </h3>
            {subtitle ? (
              <p className="truncate text-[11px] text-muted-foreground">{subtitle}</p>
            ) : null}
          </div>
        </header>
      ) : null}
      {children}
    </section>
  );
}

/// RAG 引用元カード。
export function GenUiSourceCard({ card }: { card: SourceCardProps }) {
  const sources = card.sources ?? [];
  return (
    <DomainCard icon={<Quote className="size-4" />} title={card.title || "出典"} testId="genui-source-card">
      <ul>
        {sources.map((s, i) => (
          <li
            key={i}
            className={cn("px-3.5 py-2.5", i < sources.length - 1 && "shiki-dash-bottom")}
          >
            <div className="flex items-start justify-between gap-2">
              {/* 未検証 payload（note 埋め込み等）でも javascript:/data: を踏ませないよう
                  レンダラ側でも https を再確認する（link コンポーネントと同じ防御）。 */}
              {s.url && s.url.startsWith("https://") ? (
                <a
                  href={s.url}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="inline-flex min-w-0 items-center gap-1 text-[13px] font-medium text-foreground hover:text-primary hover:underline"
                >
                  <span className="truncate">{s.title}</span>
                  <ExternalLink className="size-3 shrink-0 text-muted-foreground" aria-hidden />
                </a>
              ) : (
                <span className="min-w-0 truncate text-[13px] font-medium text-foreground">
                  {s.title}
                </span>
              )}
              {typeof s.score === "number" && Number.isFinite(s.score) ? (
                <span
                  className="shrink-0 rounded-full px-1.5 py-0.5 text-[10px] font-semibold tabular-nums"
                  style={{
                    backgroundColor: "color-mix(in oklab, var(--season) 14%, transparent)",
                    color: "var(--season)",
                  }}
                >
                  {s.score.toLocaleString("ja-JP", { maximumFractionDigits: 2 })}
                </span>
              ) : null}
            </div>
            {s.snippet ? (
              <p className="mt-1 line-clamp-2 text-[12px] leading-relaxed text-muted-foreground">
                {s.snippet}
              </p>
            ) : null}
            {s.label ? (
              <span className="mt-1 inline-block text-[10px] font-medium uppercase tracking-wide text-muted-foreground/70">
                {s.label}
              </span>
            ) : null}
          </li>
        ))}
      </ul>
    </DomainCard>
  );
}

/// 旅程の予定種別 → アイコン＋差し色。
const ITINERARY_META: Record<ItineraryKind, { icon: React.ElementType; color: string }> = {
  activity: { icon: Circle, color: "var(--season)" },
  travel: { icon: Navigation, color: "var(--season-winter)" },
  food: { icon: Utensils, color: "var(--season-autumn)" },
  lodging: { icon: BedDouble, color: "var(--season-spring)" },
  sight: { icon: Camera, color: "var(--season-summer)" },
};

/// 旅程カード（日ごとの縦タイムライン）。
export function GenUiItinerary({ itinerary }: { itinerary: ItineraryProps }) {
  const days = itinerary.days ?? [];
  return (
    <DomainCard
      icon={<MapPin className="size-4" />}
      title={itinerary.title || "旅程"}
      testId="genui-itinerary"
    >
      <div className="flex flex-col gap-4 px-3.5 py-3">
        {days.map((day, di) => (
          <div key={di} className="min-w-0">
            {(day.label || day.date) && (
              <div className="mb-2 flex items-baseline gap-2">
                {day.label ? (
                  <span className="text-[12px] font-semibold text-foreground">{day.label}</span>
                ) : null}
                {day.date ? (
                  <span className="text-[11px] text-muted-foreground">{day.date}</span>
                ) : null}
              </div>
            )}
            <ol className="space-y-0">
              {(day.items ?? []).map((it, ii, arr) => {
                const meta = ITINERARY_META[it.kind] ?? ITINERARY_META.activity;
                const Icon = meta.icon;
                const last = ii === arr.length - 1;
                return (
                  <li key={ii} className="flex gap-2.5">
                    <div className="flex w-10 shrink-0 justify-end pt-0.5 text-[11px] tabular-nums text-muted-foreground">
                      {it.time ?? ""}
                    </div>
                    <div className="flex flex-col items-center">
                      <span
                        className="grid size-6 shrink-0 place-items-center rounded-full border bg-card"
                        style={{ borderColor: meta.color, color: meta.color }}
                        aria-hidden
                      >
                        <Icon className="size-3" />
                      </span>
                      {!last ? <span className="w-px flex-1 bg-border" aria-hidden /> : null}
                    </div>
                    <div className="min-w-0 pb-3">
                      <p className="text-[13px] font-medium text-foreground">{it.title}</p>
                      {it.location ? (
                        <p className="mt-0.5 flex items-center gap-1 text-[11px] text-muted-foreground">
                          <MapPin className="size-3 shrink-0" aria-hidden />
                          {it.location}
                        </p>
                      ) : null}
                      {it.description ? (
                        <p className="mt-0.5 text-[12px] leading-relaxed text-muted-foreground">
                          {it.description}
                        </p>
                      ) : null}
                    </div>
                  </li>
                );
              })}
            </ol>
          </div>
        ))}
      </div>
    </DomainCard>
  );
}

/// 天候 → アイコン＋色＋読み上げ用ラベル（色だけに頼らず condition を伝える）。
const WEATHER_META: Record<WeatherCondition, { icon: React.ElementType; cls: string; label: string }> = {
  sunny: { icon: Sun, cls: "text-amber-500", label: "晴れ" },
  partly_cloudy: { icon: CloudSun, cls: "text-amber-500/80", label: "晴れ時々くもり" },
  cloudy: { icon: Cloud, cls: "text-muted-foreground", label: "くもり" },
  rain: { icon: CloudRain, cls: "text-[var(--season-winter)]", label: "雨" },
  storm: { icon: CloudLightning, cls: "text-[var(--season-winter)]", label: "雷雨" },
  snow: { icon: Snowflake, cls: "text-sky-400", label: "雪" },
  fog: { icon: CloudFog, cls: "text-muted-foreground/70", label: "霧" },
};

/// 天気カード（地点＋日別の天候）。
export function GenUiWeather({ weather }: { weather: WeatherProps }) {
  const days = weather.days ?? [];
  const fmt = (v: number | null | undefined) =>
    typeof v === "number" && Number.isFinite(v) ? `${Math.round(v)}°` : "–";
  return (
    <DomainCard
      icon={<CloudSun className="size-4" />}
      title={weather.title || weather.location}
      // title を明示した場合でも地点情報を失わないよう副題に location を出す。
      subtitle={weather.title ? weather.location : null}
      testId="genui-weather"
    >
      <div className="grid grid-cols-2 gap-2 p-3 sm:grid-cols-3 md:grid-cols-4">
        {days.map((d, i) => {
          const meta = WEATHER_META[d.condition] ?? WEATHER_META.cloudy;
          const Icon = meta.icon;
          return (
            <div
              key={i}
              className="flex flex-col items-center gap-1 rounded-lg border border-border/60 bg-card/40 px-2 py-2.5"
            >
              <span className="text-[11px] font-medium text-muted-foreground">{d.label}</span>
              <Icon className={cn("size-6", meta.cls)} role="img" aria-label={meta.label} />
              {/* アイコンだけだと読み上げ/高コントラストで天候が伝わらないため補う。 */}
              <span className="sr-only">{meta.label}</span>
              <div className="flex items-baseline gap-1 tabular-nums">
                <span className="text-[13px] font-semibold text-foreground">{fmt(d.high)}</span>
                <span className="text-[11px] text-muted-foreground">{fmt(d.low)}</span>
              </div>
              {typeof d.precipitation === "number" && Number.isFinite(d.precipitation) ? (
                <span className="flex items-center gap-0.5 text-[10px] text-[var(--season-winter)]">
                  <CloudRain className="size-2.5" aria-hidden />
                  {Math.round(d.precipitation)}%
                </span>
              ) : null}
            </div>
          );
        })}
      </div>
    </DomainCard>
  );
}

/// 比較カード（2〜N 列の項目別比較・推し列は塗りで強調）。
export function GenUiComparison({ comparison }: { comparison: ComparisonProps }) {
  const columns = comparison.columns ?? [];
  const rows = comparison.rows ?? [];
  const hi = comparison.highlight ?? null;
  return (
    <DomainCard
      icon={<GitCompare className="size-4" />}
      title={comparison.title || "比較"}
      testId="genui-comparison"
    >
      <div className="overflow-x-auto">
        <table className="w-full border-collapse text-[13px]">
          <thead>
            <tr className="shiki-dash-bottom">
              <th className="px-3.5 py-2 text-left text-[11px] font-semibold uppercase tracking-[0.06em] text-muted-foreground/70" />
              {columns.map((c, i) => (
                <th
                  key={i}
                  className={cn(
                    "px-3 py-2 text-left font-semibold tracking-tight text-foreground",
                    hi === i && "bg-accent",
                  )}
                >
                  {c}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((row, ri) => (
              <tr key={ri} className={cn(ri < rows.length - 1 && "shiki-dash-bottom")}>
                <th className="px-3.5 py-2 text-left align-top text-[12px] font-medium text-muted-foreground">
                  {row.label}
                </th>
                {(row.values ?? []).map((v, vi) => (
                  <td
                    key={vi}
                    className={cn(
                      "px-3 py-2 align-top text-foreground",
                      hi === vi && "bg-accent font-medium",
                    )}
                  >
                    {v}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </DomainCard>
  );
}

/// タイムラインのトーン → ドット色。
const TIMELINE_TONE: Record<string, string> = {
  neutral: "bg-muted-foreground/40",
  info: "bg-primary",
  success: "bg-[var(--season-summer)]",
  warning: "bg-amber-500",
  danger: "bg-destructive",
};

/// タイムライン（時系列イベント列）。
export function GenUiTimeline({ timeline }: { timeline: TimelineProps }) {
  const events = timeline.events ?? [];
  return (
    <DomainCard
      icon={<Clock className="size-4" />}
      title={timeline.title || "タイムライン"}
      testId="genui-timeline"
    >
      <ol className="space-y-0 px-3.5 py-3">
        {events.map((e, i) => {
          const last = i === events.length - 1;
          return (
            <li key={i} className="flex gap-3">
              <div className="flex flex-col items-center pt-1">
                <span
                  className={cn("size-2.5 shrink-0 rounded-full", TIMELINE_TONE[e.tone] ?? TIMELINE_TONE.neutral)}
                  aria-hidden
                />
                {!last ? <span className="mt-0.5 w-px flex-1 bg-border" aria-hidden /> : null}
              </div>
              <div className="min-w-0 pb-3.5">
                {e.time ? (
                  <span className="text-[11px] tabular-nums text-muted-foreground">{e.time}</span>
                ) : null}
                <p className="text-[13px] font-medium text-foreground">{e.title}</p>
                {e.description ? (
                  <p className="mt-0.5 text-[12px] leading-relaxed text-muted-foreground">
                    {e.description}
                  </p>
                ) : null}
              </div>
            </li>
          );
        })}
      </ol>
    </DomainCard>
  );
}
