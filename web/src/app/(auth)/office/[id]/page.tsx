"use client";

/// Office 文書編集ページ（Task 11.7）。/office/[id] で docx/xlsx/pptx を Collabora
/// iframe で開く。
///
/// - `/office/sessions` で編集アクション URL＋WOPI access_token＋WOPISrc を取得し、
///   **form POST** で iframe に注入する（WOPI 標準の起動手順。トークンを URL クエリに
///   載せずボディで渡す＝アクセスログへの漏出を避ける）。
/// - トークンは入場券にすぎず、権限は WOPI 側が毎呼び出しで ReBAC 再チェックする
///   （共有解除は即時反映・PIT-11）。
/// - Collabora 未配備（office profile 未起動）は 503 → 案内表示にフォールバックする。

import { FileWarning, Loader2, PlugZap } from "lucide-react";
import { useParams, useRouter } from "next/navigation";
import * as React from "react";

import { EmptyState } from "@/components/ui/empty-state";
import {
  buildOfficeFrameUrl,
  createOfficeSession,
  OfficeSessionError,
  type OfficeSession,
} from "@/lib/office-api";

type LoadState =
  | { phase: "loading" }
  | { phase: "ready"; session: OfficeSession }
  | { phase: "notfound" }
  | { phase: "unavailable" }
  | { phase: "error"; message: string };

export default function OfficePage() {
  const params = useParams<{ id: string }>();
  const fileId = params.id;
  const router = useRouter();
  const [state, setState] = React.useState<LoadState>({ phase: "loading" });
  // Collabora の Frame_Ready まではスピナーを重ねる（白画面のちらつきを見せない）。
  const [frameReady, setFrameReady] = React.useState(false);
  const iframeRef = React.useRef<HTMLIFrameElement>(null);

  React.useEffect(() => {
    let cancelled = false;
    createOfficeSession(fileId)
      .then((session) => {
        if (!cancelled) setState({ phase: "ready", session });
      })
      .catch((e) => {
        if (cancelled) return;
        if (e instanceof OfficeSessionError) {
          setState({ phase: e.kind });
        } else {
          setState({ phase: "error", message: e instanceof Error ? e.message : String(e) });
        }
      });
    return () => {
      cancelled = true;
    };
  }, [fileId]);

  // Collabora からの postMessage（CheckFileInfo の PostMessageOrigin 宛て）。
  // Frame_Ready で handshake（Host_PostmessageReady）を返し、UI_Close でドライブへ戻る。
  React.useEffect(() => {
    if (state.phase !== "ready") return;
    const collaboraOrigin = new URL(state.session.action_url).origin;
    const onMessage = (event: MessageEvent) => {
      if (event.origin !== collaboraOrigin) return;
      let msg: { MessageId?: string } & Record<string, unknown>;
      try {
        msg = typeof event.data === "string" ? JSON.parse(event.data) : event.data;
      } catch {
        return;
      }
      if (msg.MessageId === "App_LoadingStatus") {
        const values = msg.Values as { Status?: string } | undefined;
        if (values?.Status === "Frame_Ready") {
          setFrameReady(true);
          iframeRef.current?.contentWindow?.postMessage(
            JSON.stringify({ MessageId: "Host_PostmessageReady" }),
            collaboraOrigin,
          );
        }
      } else if (msg.MessageId === "UI_Close") {
        router.push("/drive");
      }
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [state, router]);

  // セッション取得後、非表示 form を iframe へ POST してエディタを起動する。
  const formRef = React.useRef<HTMLFormElement>(null);
  React.useEffect(() => {
    if (state.phase === "ready") formRef.current?.submit();
  }, [state]);

  if (state.phase === "loading") {
    return (
      <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        文書を開いています…
      </div>
    );
  }
  if (state.phase === "notfound") {
    return (
      <EmptyState
        icon={FileWarning}
        title="この文書は開けません"
        description="ファイルが存在しないか、開く権限がないか、ブラウザ編集に対応していない形式です。"
      />
    );
  }
  if (state.phase === "unavailable") {
    return (
      <EmptyState
        icon={PlugZap}
        title="Office 編集サービスに接続できません"
        description="Collabora が起動していません。管理者に office profile の有効化（docker compose --profile office up）を確認してください。"
      />
    );
  }
  if (state.phase === "error") {
    return <EmptyState icon={FileWarning} title="読み込みに失敗しました" description={state.message} />;
  }

  const frameUrl = buildOfficeFrameUrl(state.session);
  return (
    <div className="relative h-full min-h-0">
      {/* WOPI 標準の起動: access_token を form POST で渡す（URL に載せない）。 */}
      <form
        ref={formRef}
        action={frameUrl}
        method="post"
        target="office-frame"
        className="hidden"
        aria-hidden
      >
        <input name="access_token" value={state.session.access_token} type="hidden" readOnly />
        <input
          name="access_token_ttl"
          value={String(Date.now() + state.session.access_token_ttl_ms)}
          type="hidden"
          readOnly
        />
      </form>
      {!frameReady ? (
        <div className="absolute inset-0 z-10 flex items-center justify-center gap-2 bg-background text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" aria-hidden />
          エディタを起動しています…
        </div>
      ) : null}
      <iframe
        ref={iframeRef}
        name="office-frame"
        title="Office 文書エディタ"
        data-testid="office-frame"
        className="h-full w-full border-0"
        allow="clipboard-read; clipboard-write"
      />
    </div>
  );
}
