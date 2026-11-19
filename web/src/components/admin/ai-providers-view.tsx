"use client";

import * as React from "react";
import { AlertCircle, CircleCheck, CircleMinus, Globe2, KeyRound, Server, TriangleAlert } from "lucide-react";

import {
  fetchAdminProviders,
  fetchProviderStatus,
  type AdminProvider,
  type AdminProviderHealth,
  type ProviderHealthReason,
  type ProviderHealthState,
  type ProviderStatusResponse,
} from "@/lib/admin-ai-api";
import { Badge } from "@/components/ui/badge";
import { EmptyState } from "@/components/ui/empty-state";
import { Skeleton } from "@/components/ui/skeleton";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { AdminPage } from "./admin-page";

const KIND_LABEL: Record<string, string> = {
  anthropic: "Anthropic API",
  openai: "OpenAI 互換（Chat Completions）",
  openai_responses: "OpenAI Responses API",
  chatgpt_plan: "ChatGPT プラン",
  stub: "スタブ（開発・テスト用）",
};

/// 状態の更新間隔（サーバの書き出しは 15 秒ごと）。
const REFRESH_MS = 30_000;

const STATE_LABEL: Record<ProviderHealthState, string> = {
  operational: "正常",
  degraded: "不安定",
  down: "停止",
  idle: "利用なし",
  unmonitored: "対象外",
};

const STATE_BADGE: Record<ProviderHealthState, "success" | "warning" | "destructive" | "muted"> = {
  operational: "success",
  degraded: "warning",
  down: "destructive",
  idle: "muted",
  unmonitored: "muted",
};

const STATE_ICON: Record<ProviderHealthState, typeof CircleCheck> = {
  operational: CircleCheck,
  degraded: TriangleAlert,
  down: AlertCircle,
  idle: CircleMinus,
  unmonitored: CircleMinus,
};

/// 稼働バーの 1 本の色（状態は色だけでなく凡例とツールチップの文でも伝える）。
const BAR_COLOR: Record<ProviderHealthState, string> = {
  operational: "bg-[oklch(0.72_0.13_150)] dark:bg-[oklch(0.62_0.12_150)]",
  degraded: "bg-[oklch(0.8_0.14_80)] dark:bg-[oklch(0.72_0.13_80)]",
  down: "bg-[oklch(0.63_0.19_25)] dark:bg-[oklch(0.6_0.17_25)]",
  // ライトではカードの地と見分けにくいので、muted より一段濃くする。
  idle: "bg-foreground/10",
  unmonitored: "bg-foreground/10",
};

const REASON_TEXT: Record<ProviderHealthReason, string> = {
  errors: "失敗する呼び出しが増えています",
  rate_limited: "利用上限（レート制限）に当たっています",
  slow: "最初の応答が遅くなっています",
};

const timeFmt = new Intl.DateTimeFormat("ja-JP", { hour: "2-digit", minute: "2-digit" });
const dateTimeFmt = new Intl.DateTimeFormat("ja-JP", {
  month: "numeric",
  day: "numeric",
  hour: "2-digit",
  minute: "2-digit",
});

/// 0〜1 → `99.8%`（100% 未満を 100.0% に丸めない）。
function formatRate(r: number): string {
  // サーバは 1 - 失敗率で返すので 0.93 が 0.9299… になる。丸め誤差の分だけ足してから切り捨てる。
  const pct = Math.floor(r * 1000 + 1e-6) / 10;
  return `${pct.toFixed(1)}%`;
}

function formatSeconds(ms: number): string {
  return ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(ms < 10_000 ? 1 : 0)} 秒`;
}

function StateBadge({ state }: { state: ProviderHealthState }) {
  const Icon = STATE_ICON[state];
  return (
    <Badge variant={STATE_BADGE[state]} className="gap-1 px-2 py-0.5">
      <Icon className="size-3.5" aria-hidden />
      {STATE_LABEL[state]}
    </Badge>
  );
}

/// 率・時間が返らないときの表示。サーバは利用が少ない窓の率を返さない（集計は全テナント共有で、
/// 分母が小さい率は他の組織の件数そのものになるため）。
const FEW = "件数が少ないため非表示";

/// 直近 24 時間の稼働（1 時間 1 本）。
function UptimeBars({ health }: { health: AdminProviderHealth }) {
  const hours = health.hours;
  const used = hours.some((h) => h.state !== "idle");
  const bad = hours.filter((h) => h.state === "degraded" || h.state === "down").length;
  const rate =
    health.availability_24h != null
      ? `稼働率 ${formatRate(health.availability_24h)}`
      : used
        ? FEW
        : "利用なし";
  const summary = `直近 24 時間: ${rate}${bad > 0 ? `、不安定・停止のあった時間 ${bad} 時間` : ""}`;
  return (
    <div className="px-4 pb-3.5 pt-3">
      <div className="mb-2 flex items-baseline justify-between gap-2 text-xs">
        <span className="text-muted-foreground">直近 24 時間</span>
        <span className="tabular-nums text-muted-foreground">
          {health.availability_24h == null ? (
            rate
          ) : (
            <>
              稼働率 <span className="font-medium text-foreground">{formatRate(health.availability_24h)}</span>
            </>
          )}
        </span>
      </div>
      <div role="img" aria-label={summary} className="flex h-7 items-stretch gap-[3px]">
        {hours.map((h) => {
          const start = new Date(h.start);
          const end = new Date(start.getTime() + 3_600_000);
          return (
            <Tooltip key={h.start}>
              <TooltipTrigger asChild>
                <span
                  aria-hidden
                  className={cn("min-w-0 flex-1 rounded-[3px] transition-opacity hover:opacity-75", BAR_COLOR[h.state])}
                />
              </TooltipTrigger>
              <TooltipContent>
                {timeFmt.format(start)}〜{timeFmt.format(end)}・{STATE_LABEL[h.state]}
                {h.availability != null ? `（稼働率 ${formatRate(h.availability)}）` : ""}
              </TooltipContent>
            </Tooltip>
          );
        })}
      </div>
      <div className="mt-1.5 flex justify-between text-[11px] text-muted-foreground" aria-hidden>
        <span>24 時間前</span>
        <span>今</span>
      </div>
    </div>
  );
}

function Metric({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="min-w-0 px-4 py-3">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="mt-1 truncate tabular-nums">{children}</dd>
    </div>
  );
}

function ProviderCard({
  p,
  health,
  windowMinutes,
}: {
  p: AdminProvider;
  /// 稼働状況（読み込み中・読めなかったときは undefined）。
  health: AdminProviderHealth | undefined;
  windowMinutes: number;
}) {
  const monitored = health != null && health.state !== "unmonitored";
  return (
    <section aria-label={p.label} className="rounded-lg border bg-card">
      <div className="flex flex-wrap items-center gap-3 px-4 py-3.5">
        <span className="flex size-9 items-center justify-center rounded-lg bg-muted text-sm font-semibold">
          {p.label.slice(0, 1).toUpperCase()}
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-sm font-semibold">{p.label}</span>
            <Badge variant="muted" className="px-1.5 py-0 text-[11px]">
              {KIND_LABEL[p.kind] ?? p.kind}
            </Badge>
            {p.processed_abroad ? (
              <Badge variant="warning" className="gap-1 px-1.5 py-0 text-[11px]">
                <Globe2 className="size-3" aria-hidden />
                国外処理
              </Badge>
            ) : null}
          </div>
          <div className="mt-0.5 truncate font-mono text-xs text-muted-foreground">
            {p.endpoint ?? "既定のエンドポイント"}
          </div>
        </div>
        {health ? <StateBadge state={health.state} /> : null}
      </div>
      {health && health.reasons.length > 0 ? (
        <ul
          className={cn(
            "mx-4 mb-1 flex flex-col gap-0.5 rounded-md px-3 py-2 text-[13px]",
            health.state === "down"
              ? "bg-[oklch(0.96_0.03_25)] text-[oklch(0.42_0.14_25)] dark:bg-[oklch(0.28_0.06_25)] dark:text-[oklch(0.86_0.08_25)]"
              : "bg-[oklch(0.97_0.04_85)] text-[oklch(0.42_0.1_70)] dark:bg-[oklch(0.3_0.05_80)] dark:text-[oklch(0.88_0.08_85)]",
          )}
        >
          {health.reasons.map((r) => (
            <li key={r}>{REASON_TEXT[r]}</li>
          ))}
        </ul>
      ) : null}
      {monitored ? <UptimeBars health={health} /> : null}
      {health?.state === "unmonitored" ? (
        <p className="px-4 pb-3 text-[13px] text-muted-foreground">
          連携した本人のプラン枠で動くため、稼働状況は数えていません。
        </p>
      ) : null}
      <dl className="shiki-dash-top grid grid-cols-2 text-[13px] sm:grid-cols-3">
        <Metric label="ID">
          <span className="font-mono text-xs">{p.id}</span>
        </Metric>
        <Metric label="モデル">{p.model_count}</Metric>
        <Metric label="API キー">
          <span className="flex items-center gap-1.5">
            <KeyRound className="size-3.5 text-muted-foreground" aria-hidden />
            {p.has_api_key ? "設定済み" : "なし"}
          </span>
        </Metric>
        {monitored ? (
          <>
            <Metric label={`失敗率（直近 ${windowMinutes} 分）`}>
              {health.failure_rate != null
                ? formatRate(health.failure_rate)
                : health.state === "idle"
                  ? "—"
                  : FEW}
            </Metric>
            <Metric label="最初の応答（p95）">
              {health.ttft_p95_ms != null
                ? `${formatSeconds(health.ttft_p95_ms)} 以内`
                : health.state === "idle"
                  ? "—"
                  : FEW}
            </Metric>
            <Metric label="最後の成功">
              {health.last_ok_at ? dateTimeFmt.format(new Date(health.last_ok_at)) : "直近 7 日はなし"}
            </Metric>
          </>
        ) : null}
      </dl>
    </section>
  );
}

/// 一覧の上の 1 行のまとめ。
function Summary({ status }: { status: ProviderStatusResponse }) {
  const monitored = status.providers.filter((p) => p.state !== "unmonitored");
  const down = monitored.filter((p) => p.state === "down").length;
  const degraded = monitored.filter((p) => p.state === "degraded").length;
  const active = monitored.filter((p) => p.state !== "idle").length;
  const state: ProviderHealthState =
    down > 0 ? "down" : degraded > 0 ? "degraded" : active > 0 ? "operational" : "idle";
  const text =
    down > 0
      ? `${down} 件のプロバイダが停止しています`
      : degraded > 0
        ? `${degraded} 件のプロバイダが不安定です`
        : active > 0
          ? "すべてのプロバイダが正常に応答しています"
          : `直近 ${status.window_minutes} 分の利用がないため、状態を判定できません`;
  const Icon = STATE_ICON[state];
  return (
    <div
      role="status"
      className={cn(
        "flex flex-wrap items-center gap-x-3 gap-y-1 rounded-lg border px-4 py-3",
        state === "down" && "border-[oklch(0.85_0.07_25)] dark:border-[oklch(0.42_0.1_25)]",
        state === "degraded" && "border-[oklch(0.86_0.08_80)] dark:border-[oklch(0.45_0.08_80)]",
      )}
    >
      <Icon
        className={cn(
          "size-5 shrink-0",
          state === "operational" && "text-[oklch(0.6_0.13_150)]",
          state === "degraded" && "text-[oklch(0.68_0.14_75)]",
          state === "down" && "text-[oklch(0.58_0.19_25)]",
          state === "idle" && "text-muted-foreground",
        )}
        aria-hidden
      />
      {/* 狭い幅では時刻を次の行へ回し、見出しを語の途中で折らない。 */}
      <span className="min-w-[12rem] flex-1 text-sm font-medium">{text}</span>
      <span className="pl-8 text-xs tabular-nums text-muted-foreground sm:pl-0">
        {timeFmt.format(new Date(status.generated_at))} 時点・{REFRESH_MS / 1000} 秒ごとに更新
      </span>
    </div>
  );
}

/// 状態を定期的に読み直す（タブが見えている間だけ）。
function useProviderStatus(): { status: ProviderStatusResponse | null; error: string | null } {
  const [status, setStatus] = React.useState<ProviderStatusResponse | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  React.useEffect(() => {
    let active = true;
    const load = () =>
      fetchProviderStatus()
        .then((r) => {
          if (!active) return;
          setStatus(r);
          setError(null);
        })
        .catch((e: unknown) => active && setError(e instanceof Error ? e.message : String(e)));
    void load();
    const timer = window.setInterval(() => {
      if (document.visibilityState === "visible") void load();
    }, REFRESH_MS);
    const onVisible = () => {
      if (document.visibilityState === "visible") void load();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      active = false;
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, []);
  return { status, error };
}

/// 接続済みプロバイダの一覧と稼働状況（表示のみ。追加・キー登録はデプロイ設定で行う）。
export function AiProvidersView() {
  const [providers, setProviders] = React.useState<AdminProvider[] | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const { status, error: statusError } = useProviderStatus();

  React.useEffect(() => {
    let active = true;
    fetchAdminProviders()
      .then((r) => active && setProviders(r))
      .catch((e: unknown) => active && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      active = false;
    };
  }, []);

  const healthOf = (id: string) => status?.providers.find((h) => h.id === id);

  return (
    <AdminPage
      title="プロバイダ"
      description="LLM の接続先と稼働状況です。複数のプロバイダを同時に使え、モデルごとに振り分けます。"
    >
      <div className="flex items-start gap-2.5 rounded-lg border bg-muted/40 px-3.5 py-2.5 text-[13px] text-muted-foreground">
        <Server className="mt-0.5 size-4 shrink-0" aria-hidden />
        <p>
          接続先と API キーはデプロイ設定（<code className="font-mono text-xs">llm.providers</code>
          ）で管理します。外部 API のプロバイダは、プロンプトと参照文書の一部を日本国外で処理する場合があり、該当するモデルにはチャットでも「国外処理」と表示されます。
        </p>
      </div>
      {error ? (
        <EmptyState icon={AlertCircle} title="プロバイダを読み込めませんでした" description={error} />
      ) : !providers ? (
        <div className="flex flex-col gap-3" aria-busy="true">
          <Skeleton className="h-12" />
          <Skeleton className="h-44" />
          <Skeleton className="h-44" />
        </div>
      ) : (
        <div className="flex flex-col gap-3">
          {status ? (
            <>
              <Summary status={status} />
              {statusError ? (
                <div
                  role="alert"
                  className="flex items-center gap-2.5 rounded-lg border px-4 py-2.5 text-[13px] text-muted-foreground"
                >
                  <AlertCircle className="size-4 shrink-0" aria-hidden />
                  最新の状態を読み込めませんでした（{statusError}）。{timeFmt.format(new Date(status.generated_at))}{" "}
                  時点の状態を表示しています。
                </div>
              ) : null}
            </>
          ) : statusError ? (
            <div
              role="alert"
              className="flex items-center gap-2.5 rounded-lg border px-4 py-3 text-[13px] text-muted-foreground"
            >
              <AlertCircle className="size-4 shrink-0" aria-hidden />
              稼働状況を読み込めませんでした（{statusError}）。30 秒後にもう一度読み込みます。
            </div>
          ) : (
            <Skeleton className="h-12" />
          )}
          {providers.map((p) => (
            <ProviderCard
              key={p.id}
              p={p}
              health={healthOf(p.id)}
              windowMinutes={status?.window_minutes ?? 5}
            />
          ))}
          <p className="text-xs leading-relaxed text-muted-foreground">
            稼働状況は、この環境（ほかの組織を含む）から各プロバイダへの実際の呼び出しの結果で判定しています。送り直しも 1 回と数え、呼び出し側の誤りによる失敗は数えません。直近の利用がないプロバイダは「利用なし」と表示し、件数が少ない間は率と時間を表示しません。
          </p>
        </div>
      )}
    </AdminPage>
  );
}
