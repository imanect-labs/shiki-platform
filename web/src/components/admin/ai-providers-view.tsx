"use client";

import * as React from "react";
import { AlertCircle, Globe2, KeyRound, Server } from "lucide-react";

import { fetchAdminProviders, type AdminProvider } from "@/lib/admin-ai-api";
import { Badge } from "@/components/ui/badge";
import { EmptyState } from "@/components/ui/empty-state";
import { Skeleton } from "@/components/ui/skeleton";
import { AdminPage } from "./admin-page";

const KIND_LABEL: Record<string, string> = {
  anthropic: "Anthropic API",
  openai: "OpenAI 互換（Chat Completions）",
  openai_responses: "OpenAI Responses API",
  stub: "スタブ（開発・テスト用）",
};

function ProviderCard({ p }: { p: AdminProvider }) {
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
      </div>
      <dl className="grid grid-cols-2 border-t text-[13px] sm:grid-cols-3">
        <div className="px-4 py-3">
          <dt className="text-xs text-muted-foreground">ID</dt>
          <dd className="mt-1 font-mono text-xs">{p.id}</dd>
        </div>
        <div className="px-4 py-3">
          <dt className="text-xs text-muted-foreground">モデル</dt>
          <dd className="mt-1 tabular-nums">{p.model_count}</dd>
        </div>
        <div className="col-span-2 px-4 py-3 sm:col-span-1">
          <dt className="text-xs text-muted-foreground">API キー</dt>
          <dd className="mt-1 flex items-center gap-1.5">
            <KeyRound className="size-3.5 text-muted-foreground" aria-hidden />
            {p.has_api_key ? "設定済み" : "なし"}
          </dd>
        </div>
      </dl>
    </section>
  );
}

/// 接続済みプロバイダの一覧（表示のみ。追加・キー登録はデプロイ設定で行う）。
export function AiProvidersView() {
  const [providers, setProviders] = React.useState<AdminProvider[] | null>(null);
  const [error, setError] = React.useState<string | null>(null);

  React.useEffect(() => {
    let active = true;
    fetchAdminProviders()
      .then((r) => active && setProviders(r))
      .catch((e: unknown) => active && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      active = false;
    };
  }, []);

  return (
    <AdminPage
      title="プロバイダ"
      description="LLM の接続先です。複数のプロバイダを同時に使え、モデルごとに振り分けます。"
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
          <Skeleton className="h-32" />
          <Skeleton className="h-32" />
        </div>
      ) : (
        <div className="flex flex-col gap-3">
          {providers.map((p) => (
            <ProviderCard key={p.id} p={p} />
          ))}
        </div>
      )}
    </AdminPage>
  );
}
