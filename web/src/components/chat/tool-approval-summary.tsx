"use client";

/// 承認カードの中身（#1339）: 生のツール名・共通の定型文・入力の JSON の代わりに、
/// **どのファイルに**（名前と今の場所）・**何をして**（操作ごとに 1 行）・**何が起きるか**（副作用・取り消せるか）を見せる。
///
/// 中身の組み立ては `lib/approval-detail.ts`。ファイル・フォルダの名前と場所は承認する人本人の権限で引く
/// （読めなければ名前を出さない）。表示のための読み取りで、実行してよいかはツールと StorageService が改めて確かめる。

import { Folder, Info, Loader2, TriangleAlert } from "lucide-react";
import * as React from "react";

import type { ApprovalRequest } from "@/lib/chat-api";
import { approvalDetail, type ApprovalDetail } from "@/lib/approval-detail";
import { breadcrumb, type CrumbResponse } from "@/lib/storage";
import { describeTool } from "@/lib/tool-display";

type Resolved = {
  loading: boolean;
  /// 対象のパンくず（末尾が自身）。読めなければ null。
  target: CrumbResponse[] | null;
  /// 保存先・複製先のパンくず。読めなければ null。
  folder: CrumbResponse[] | null;
};

function pathOf(crumbs: CrumbResponse[]): string {
  return crumbs.map((c) => c.name).join(" / ");
}

function useResolved(detail: ApprovalDetail): Resolved {
  const nodeId = detail.nodeId;
  const folderId = detail.folder?.id ?? null;
  const [state, setState] = React.useState<Resolved>({
    loading: nodeId !== null || folderId !== null,
    target: null,
    folder: null,
  });
  React.useEffect(() => {
    if (!nodeId && !folderId) {
      setState({ loading: false, target: null, folder: null });
      return;
    }
    let active = true;
    setState((s) => ({ ...s, loading: true }));
    void (async () => {
      const [target, folder] = await Promise.all([
        nodeId ? breadcrumb(nodeId).catch(() => null) : Promise.resolve(null),
        folderId ? breadcrumb(folderId).catch(() => null) : Promise.resolve(null),
      ]);
      if (active) setState({ loading: false, target, folder });
    })();
    return () => {
      active = false;
    };
  }, [nodeId, folderId]);
  return state;
}

/// 汎用の中身（専用の中身が無いツールは理由と送る内容だけを出す）。
export function ToolApprovalSummary({ request }: { request: ApprovalRequest }) {
  const detail = React.useMemo(() => approvalDetail(request.name, request.input), [request.name, request.input]);
  if (!detail) return <FallbackSummary request={request} />;
  return <DetailSummary request={request} detail={detail} />;
}

function FallbackSummary({ request }: { request: ApprovalRequest }) {
  const d = describeTool(request);
  return (
    <div className="mb-2 space-y-1 text-[13px]" data-testid="tool-approval-summary">
      <p className="font-medium text-foreground [overflow-wrap:anywhere]">{future(d)}</p>
      {request.reason ? <p className="text-[12px] text-muted-foreground">{request.reason}</p> : null}
    </div>
  );
}

/// 「〈対象〉を〈動詞〉します」（サ変）／「〈対象〉に書き込みます」（和語）。
function future(d: ReturnType<typeof describeTool>): string {
  return `${d.lead}${d.verb}${d.suru ? "します" : "ます"}`;
}

function DetailSummary({ request, detail }: { request: ApprovalRequest; detail: ApprovalDetail }) {
  const resolved = useResolved(detail);
  const self = resolved.target?.[resolved.target.length - 1];
  const d = describeTool(request, self?.name ?? null);
  const Icon = d.icon;
  // 今の場所は自身を除いた祖先（読める祖先が無ければ出さない）。
  const location = resolved.target && resolved.target.length > 1 ? pathOf(resolved.target.slice(0, -1)) : null;
  const folder = detail.folder
    ? detail.folder.id
      ? resolved.folder
        ? pathOf(resolved.folder)
        : null
      : detail.folder.fallback
    : null;

  return (
    <div className="mb-2 space-y-2.5 text-[13px]" data-testid="tool-approval-summary">
      {/* 対象: どのファイルに */}
      <div className="space-y-1">
        <p className="flex items-start gap-1.5 font-medium text-foreground">
          <Icon className="mt-0.5 size-4 shrink-0 text-primary" aria-hidden />
          <span className="min-w-0 [overflow-wrap:anywhere]" data-testid="approval-headline">
            {future(d)}
          </span>
        </p>
        {resolved.loading ? (
          <p className="flex items-center gap-1.5 pl-5.5 text-[12px] text-muted-foreground">
            <Loader2 className="size-3.5 animate-spin" aria-hidden />
            ファイルの場所を確かめています…
          </p>
        ) : (
          <>
            {location ? (
              <p className="flex items-start gap-1.5 pl-5.5 text-[12px] text-muted-foreground" data-testid="approval-location">
                <Folder className="mt-px size-3.5 shrink-0" aria-hidden />
                <span className="min-w-0 [overflow-wrap:anywhere]">{location}</span>
              </p>
            ) : null}
            {detail.folder && folder ? (
              <p className="flex items-start gap-1.5 pl-5.5 text-[12px] text-muted-foreground" data-testid="approval-folder">
                <Folder className="mt-px size-3.5 shrink-0" aria-hidden />
                <span className="min-w-0 [overflow-wrap:anywhere]">
                  {detail.folder.label}: <span className="font-medium text-foreground">{folder}</span>
                </span>
              </p>
            ) : null}
          </>
        )}
      </div>

      {/* すること: 何をしようとしているか */}
      {detail.summary || detail.steps.length > 0 || detail.code ? (
        <section aria-label="すること" className="space-y-1">
          <h4 className="text-[11px] font-semibold text-muted-foreground">すること</h4>
          {detail.summary ? (
            <p
              className="rounded-md border-l-2 border-primary/50 bg-background/60 px-2 py-1 text-foreground [overflow-wrap:anywhere]"
              data-testid="approval-change-summary"
            >
              {detail.summary}
            </p>
          ) : null}
          {detail.steps.length > 0 ? (
            <ul className="space-y-0.5 text-[12px]" data-testid="approval-steps">
              {detail.steps.map((s, i) => (
                <li
                  // 同じ文言の操作が並ぶことがあるので位置で区別する（並びは入力のまま変わらない）。
                  key={i}
                  className={`flex items-start gap-1.5 [overflow-wrap:anywhere] ${
                    s.destructive ? "text-destructive" : "text-foreground/80"
                  }`}
                  data-destructive={s.destructive ? "true" : undefined}
                >
                  <span aria-hidden className="mt-[7px] size-1 shrink-0 rounded-full bg-current" />
                  <span className="min-w-0">{s.text}</span>
                </li>
              ))}
              {detail.more > 0 ? (
                <li className="pl-2.5 text-muted-foreground">ほか {detail.more} 件</li>
              ) : null}
            </ul>
          ) : null}
          {detail.code ? (
            <div>
              <p className="text-[11px] text-muted-foreground">{detail.code.label}</p>
              <pre className="mt-0.5 max-h-32 overflow-auto overscroll-contain whitespace-pre-wrap rounded-lg bg-secondary/60 p-2 text-[11px] text-foreground/80 [overflow-wrap:anywhere]">
                {detail.code.text}
              </pre>
            </div>
          ) : null}
        </section>
      ) : null}

      {/* 起きること: 影響・副作用 */}
      {detail.effects.length > 0 ? (
        <section aria-label="起きること" className="space-y-1">
          <h4 className="text-[11px] font-semibold text-muted-foreground">起きること</h4>
          <ul className="space-y-0.5 text-[12px]" data-testid="approval-effects">
            {detail.effects.map((e) => (
              <li
                key={e.text}
                className={`flex items-start gap-1.5 ${e.tone === "warn" ? "text-amber-700 dark:text-amber-300" : "text-muted-foreground"}`}
                data-tone={e.tone}
              >
                {e.tone === "warn" ? (
                  <TriangleAlert className="mt-px size-3.5 shrink-0" aria-hidden />
                ) : (
                  <Info className="mt-px size-3.5 shrink-0" aria-hidden />
                )}
                <span className="min-w-0">{e.text}</span>
              </li>
            ))}
          </ul>
        </section>
      ) : null}
    </div>
  );
}

