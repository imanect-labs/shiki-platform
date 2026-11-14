"use client";

/// AI が作った・直したテーブル・保存ビュー・ミニアプリ・ワークフローへの参照カード（builder_ref ブロック・D-204）。
///
/// 中身（`{kind, id, name, version}`）は backend の `chat::builder_obs::BuilderRef` から生成した型。カードからそれぞれの
/// 画面を開く。保存ビューはテーブルの画面の中で開くので、押したときにビューを読んでテーブルを引いてから移る。

import * as React from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { AppWindow, ArrowRight, Database, Loader2, Table2, Workflow, type LucideIcon } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import type { BuilderKind, BuilderRef } from "@/lib/chat-api";
import { getView } from "@/lib/data-api";

const KINDS: Record<BuilderKind, { label: string; open: string; icon: LucideIcon }> = {
  workflow: { label: "ワークフロー", open: "エディタで開く", icon: Workflow },
  data_table: { label: "テーブル", open: "テーブルを開く", icon: Database },
  data_view: { label: "保存ビュー", open: "ビューを開く", icon: Table2 },
  mini_app: { label: "ミニアプリ", open: "アプリを開く", icon: AppWindow },
};

/// 参照 JSON を防御的に読む（形が崩れていたら描かない。値は JSON のまま保存されている）。
export function parseBuilderRef(raw: unknown): BuilderRef | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as { kind?: unknown; id?: unknown; name?: unknown; version?: unknown };
  if (typeof r.kind !== "string" || !(r.kind in KINDS)) return null;
  if (typeof r.id !== "string" || typeof r.name !== "string" || typeof r.version !== "number") {
    return null;
  }
  return { kind: r.kind as BuilderKind, id: r.id, name: r.name, version: r.version };
}

/// 種類ごとの画面（保存ビューはテーブルを引くまで分からないので null）。
function hrefOf(ref: BuilderRef): string | null {
  const id = encodeURIComponent(ref.id);
  switch (ref.kind) {
    case "workflow":
      return `/workflows/${id}`;
    case "data_table":
      return `/data/${id}`;
    case "mini_app":
      return `/apps/${id}`;
    case "data_view":
      return null;
  }
}

const OPEN_CLASS =
  "inline-flex shrink-0 items-center gap-1.5 rounded-full border px-3 py-1.5 text-xs font-medium transition-colors duration-fast hover:border-primary/40 hover:bg-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-60";

/// 保存ビューを開く（ビューを読んで、そのテーブルの画面でビューを選んだ状態にする）。
function OpenViewButton({ id, label }: { id: string; label: string }) {
  const router = useRouter();
  const [state, setState] = React.useState<"idle" | "opening" | "failed">("idle");
  const open = async () => {
    setState("opening");
    try {
      const view = await getView(id);
      router.push(`/data/${encodeURIComponent(view.table_id)}?view=${encodeURIComponent(id)}`);
    } catch {
      setState("failed");
    }
  };
  return (
    <span className="flex shrink-0 flex-col items-end gap-1">
      <button type="button" className={OPEN_CLASS} onClick={open} disabled={state === "opening"}>
        {label}
        {state === "opening" ? (
          <Loader2 className="size-3.5 animate-spin" aria-hidden />
        ) : (
          <ArrowRight className="size-3.5" aria-hidden />
        )}
      </button>
      {state === "failed" ? (
        <span role="alert" className="text-xs text-destructive">
          開けませんでした
        </span>
      ) : null}
    </span>
  );
}

export function BuilderRefCard({ raw }: { raw: unknown }) {
  const ref = parseBuilderRef(raw);
  if (!ref) return null;
  const kind = KINDS[ref.kind];
  const Icon = kind.icon;
  const href = hrefOf(ref);
  return (
    <div className="my-2 flex items-center gap-3 rounded-xl border bg-card p-3" data-testid="builder-ref-card">
      <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary">
        <Icon className="size-4.5" aria-hidden />
      </span>
      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-2">
          <span className="truncate text-sm font-medium" title={ref.name}>
            {ref.name}
          </span>
          <Badge variant="muted">v{ref.version}</Badge>
        </span>
        <span className="mt-0.5 block text-xs text-muted-foreground">{kind.label}</span>
      </span>
      {href ? (
        <Link href={href} className={OPEN_CLASS}>
          {kind.open}
          <ArrowRight className="size-3.5" aria-hidden />
        </Link>
      ) : (
        <OpenViewButton id={ref.id} label={kind.open} />
      )}
    </div>
  );
}
