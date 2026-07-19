"use client";

/// 埋め込みブロックの描画（Task 11P.6・3 種のみ・生 HTML は絶対に描画しない）。

import { AlertTriangle, ExternalLink, FileText, Loader2 } from "lucide-react";
import * as React from "react";

import { SpecRenderer } from "@/components/genui/spec-renderer";
import { downloadUrl, getNode, triggerDownload, type NodeResponse } from "@/lib/storage";
import { parseEmbedPayload, type EmbedPayload } from "./types";

/// 未知・不正ペイロードのプレースホルダ（生 HTML を描画しないための最終防壁）。
function InvalidEmbed({ reason }: { reason: string }) {
  return (
    <div className="my-2 flex items-center gap-2 rounded-lg border border-dashed bg-muted/30 px-3 py-2 text-sm text-muted-foreground">
      <AlertTriangle className="size-4 shrink-0" aria-hidden />
      表示できない埋め込みです（{reason}）。
    </div>
  );
}

export function EmbedView({ payloadJson }: { payloadJson: string }) {
  const payload = React.useMemo(() => parseEmbedPayload(payloadJson), [payloadJson]);
  if (!payload) return <InvalidEmbed reason="未対応の形式" />;
  switch (payload.kind) {
    case "genui":
      return <GenuiEmbed payload={payload} />;
    case "iframe":
      return <IframeEmbed payload={payload} />;
    case "drive":
      return <DriveEmbed payload={payload} />;
  }
}

/// ①genui: 検証済みスペックを Phase 6 レンダラで描画（HTML 実行なし・静的カタログ）。
function GenuiEmbed({ payload }: { payload: Extract<EmbedPayload, { kind: "genui" }> }) {
  return (
    <div className="my-2 rounded-lg border bg-card p-3" data-testid="embed-genui">
      <SpecRenderer spec={payload.spec} />
    </div>
  );
}

/// ②iframe: ミニアプリ/artifact を**別オリジン・strict sandbox** で埋め込む。
/// `allow-same-origin` を付けない＝null オリジンで実行され、親（cookie/DOM）に触れない。
function IframeEmbed({ payload }: { payload: Extract<EmbedPayload, { kind: "iframe" }> }) {
  return (
    <figure className="my-2 overflow-hidden rounded-lg border" data-testid="embed-iframe">
      {/* 画面では実 iframe。印刷（PDF・#334）ではインタラクティブなため隠し、
          プレースホルダ（下）へ差し替える（静的化不能な埋め込みで紙面が破綻しない）。 */}
      <iframe
        src={payload.src}
        title={payload.title ?? "埋め込みアプリ"}
        // 別オリジン分離（B1 相当）: スクリプトは許すが same-origin は許さない。
        sandbox="allow-scripts allow-forms allow-popups allow-popups-to-escape-sandbox"
        referrerPolicy="no-referrer"
        loading="lazy"
        className="h-[420px] w-full bg-background print:hidden"
      />
      <div
        className="hidden items-center gap-2 px-3 py-4 text-sm text-muted-foreground print:flex"
        data-testid="embed-iframe-print"
      >
        <ExternalLink className="size-4 shrink-0" aria-hidden />
        <span>
          この埋め込み（{payload.title ?? payload.src}）は印刷に含まれません:{" "}
          {payload.src}
        </span>
      </div>
      <figcaption className="flex items-center gap-1.5 border-t px-3 py-1.5 text-xs text-muted-foreground print:hidden">
        <ExternalLink className="size-3.5" aria-hidden />
        <span className="truncate">{payload.title ?? payload.src}</span>
      </figcaption>
    </figure>
  );
}

/// ③drive: **閲覧者本人の ReBAC** で解決（getNode/downloadUrl は本人のセッション経由）。
function DriveEmbed({ payload }: { payload: Extract<EmbedPayload, { kind: "drive" }> }) {
  const [state, setState] = React.useState<
    { status: "loading" } | { status: "ok"; node: NodeResponse; url: string | null } | { status: "error" }
  >({ status: "loading" });

  React.useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const node = await getNode(payload.node_id);
        // 画像はプレビュー、それ以外はダウンロードリンクにする。
        const isImage = (node.content_type ?? "").startsWith("image/");
        const url = isImage ? (await downloadUrl(node.id)).url : null;
        if (!cancelled) setState({ status: "ok", node, url });
      } catch {
        // 権限が無い/存在しない→閲覧者本人の権限で解決できないだけ（作成者の権限は借用しない）。
        if (!cancelled) setState({ status: "error" });
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [payload.node_id]);

  if (state.status === "loading") {
    return (
      <div className="my-2 flex items-center gap-2 rounded-lg border bg-card px-3 py-2 text-sm text-muted-foreground">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        ファイルを読み込んでいます…
      </div>
    );
  }
  if (state.status === "error") {
    return <InvalidEmbed reason="アクセス権がありません" />;
  }
  const { node, url } = state;
  if (url) {
    return (
      <figure className="my-2 overflow-hidden rounded-lg border bg-card" data-testid="embed-drive">
        {/* eslint-disable-next-line @next/next/no-img-element */}
        <img src={url} alt={node.name} className="max-h-[480px] w-full object-contain" />
        <figcaption className="border-t px-3 py-1.5 text-xs text-muted-foreground">
          {node.name}
        </figcaption>
      </figure>
    );
  }
  return (
    <button
      type="button"
      onClick={() => void triggerDownload(node.id)}
      className="my-2 flex w-full items-center gap-3 rounded-lg border bg-card p-3 text-left transition-colors hover:border-primary/40"
      data-testid="embed-drive"
    >
      <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary">
        <FileText className="size-4.5" aria-hidden />
      </span>
      <span className="min-w-0 flex-1 truncate text-sm font-medium">{node.name}</span>
    </button>
  );
}
