"use client";

/// Collabora 文書エディタ（Task 11.7）。WOPI セッションを form POST で iframe に注入し、
/// postMessage で handshake / 選択テキスト取得 / クローズを仲介する。
///
/// 選択→AI（Task 11.10・Office 版）: Collabora の postMessage `Action_Copy`（Mimetype 指定）→
/// `Action_Copy_Resp`（Values.content = 選択テキスト）で現在の選択を取得する。API 名は
/// CODE 26.04 のバンドル実装で確認済み。取得できなければ null を返す（呼び出し側で案内）。

import * as React from "react";

import { EditorLoading } from "@/components/shell/editor-loading";
import { buildOfficeFrameUrl, type OfficeSession } from "@/lib/office-api";

export interface OfficeEditorHandle {
  /// 現在の選択テキストを取得する（無選択・タイムアウトは null）。
  getSelectionText: () => Promise<string | null>;
  /// AI ライブ編集: 現在の選択範囲を指定 HTML で置き換える（Collabora Action_Paste・#328）。
  /// セッション内へ注入するため CoolWSD 協調プロトコル経由で全参加者へ即反映する。
  applyLiveEdit: (html: string) => void;
}

/// 選択取得のタイムアウト（Collabora 応答なし＝無選択とみなす。初回コピーは LO kit の
/// ウォームアップで数百 ms〜1 秒かかることがあるため余裕を持つ）。
const SELECTION_TIMEOUT_MS = 4000;

export const OfficeEditor = React.forwardRef<
  OfficeEditorHandle,
  { session: OfficeSession; onClose: () => void }
>(function OfficeEditor({ session, onClose }, ref) {
  const iframeRef = React.useRef<HTMLIFrameElement>(null);
  const [frameReady, setFrameReady] = React.useState(false);
  const collaboraOrigin = React.useMemo(
    () => new URL(session.action_url).origin,
    [session.action_url],
  );
  // Action_Copy_Resp を待つ解決関数（選択取得の 1 回きりの待ち受け）。
  const selectionWaiterRef = React.useRef<((text: string | null) => void) | null>(null);

  const postToFrame = React.useCallback(
    (msg: Record<string, unknown>) => {
      iframeRef.current?.contentWindow?.postMessage(JSON.stringify(msg), collaboraOrigin);
    },
    [collaboraOrigin],
  );

  React.useEffect(() => {
    const onMessage = (event: MessageEvent) => {
      if (event.origin !== collaboraOrigin) return;
      let msg: { MessageId?: string; Values?: unknown };
      try {
        msg = typeof event.data === "string" ? JSON.parse(event.data) : event.data;
      } catch {
        return;
      }
      if (msg.MessageId === "App_LoadingStatus") {
        const status = (msg.Values as { Status?: string } | undefined)?.Status;
        if (status === "Frame_Ready") {
          postToFrame({ MessageId: "Host_PostmessageReady" });
          // Document_Loaded が届かない構成の保険として、レンダリング後にも一度隠す
          // （SidebarHide は冪等なので二重送信で問題ない）。
          window.setTimeout(
            () => postToFrame({ MessageId: "Send_UNO_Command", Values: { Command: ".uno:SidebarHide" } }),
            2000,
          );
        } else if (status === "Document_Loaded") {
          // 文書ロード完了後に右側サイドバー（スタイル/プロパティパネル）を隠す。
          // 埋め込み表示では横幅を圧迫し見栄えを損ねるため既定オフにする（ユーザーは
          // Collabora の「表示」メニューからいつでも再表示できる）。
          postToFrame({ MessageId: "Send_UNO_Command", Values: { Command: ".uno:SidebarHide" } });
        }
      } else if (msg.MessageId === "UI_Close") {
        onClose();
      } else if (msg.MessageId === "Action_Copy_Resp") {
        // content は要求 Mimetype（text/plain）のプレーンテキスト。
        const content = (msg.Values as { content?: string } | undefined)?.content ?? "";
        selectionWaiterRef.current?.(content.trim() ? content : null);
        selectionWaiterRef.current = null;
      }
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [collaboraOrigin, onClose, postToFrame]);

  React.useImperativeHandle(
    ref,
    () => ({
      getSelectionText: () =>
        new Promise<string | null>((resolve) => {
          // 先行の待ち受けがあれば解放（多重クリック対策）。
          selectionWaiterRef.current?.(null);
          selectionWaiterRef.current = resolve;
          postToFrame({
            MessageId: "Action_Copy",
            Values: { Mimetype: "text/plain;charset=utf-8" },
          });
          window.setTimeout(() => {
            if (selectionWaiterRef.current === resolve) {
              selectionWaiterRef.current = null;
              resolve(null);
            }
          }, SELECTION_TIMEOUT_MS);
        }),
      applyLiveEdit: (html: string) => {
        // Action_Paste は現在の選択範囲を Data で置き換える（無選択ならカーソル位置に挿入）。
        // セッション内注入のため CoolWSD 経由で全参加者へ即反映する（ファイル版競合を回避・#328）。
        postToFrame({
          MessageId: "Action_Paste",
          Values: { Mimetype: "text/html;charset=utf-8", Data: html },
        });
      },
    }),
    [postToFrame],
  );

  // セッション取得後、非表示 form を iframe へ POST してエディタを起動する。
  const formRef = React.useRef<HTMLFormElement>(null);
  const formSubmittedRef = React.useRef(false);
  React.useEffect(() => {
    if (!formSubmittedRef.current) {
      formSubmittedRef.current = true;
      formRef.current?.submit();
    }
  }, []);

  const frameUrl = buildOfficeFrameUrl(session);
  return (
    <div className="relative h-full min-h-0">
      {/* WOPI 標準の起動: access_token を form POST で渡す（URL に載せない）。 */}
      <form ref={formRef} action={frameUrl} method="post" target="office-frame" className="hidden" aria-hidden>
        <input name="access_token" value={session.access_token} type="hidden" readOnly />
        <input
          name="access_token_ttl"
          value={String(Date.now() + session.access_token_ttl_ms)}
          type="hidden"
          readOnly
        />
      </form>
      {!frameReady ? (
        <div className="absolute inset-0 z-10 bg-background">
          <EditorLoading kind="doc" message="エディタを起動しています…" />
        </div>
      ) : null}
      <iframe
        ref={iframeRef}
        name="office-frame"
        title="Office 文書エディタ"
        data-testid="office-frame"
        className="h-full w-full border-0"
        allow="clipboard-read; clipboard-write"
        onLoad={() => {
          // about:blank（初期 load）は同一オリジンで href が読める＝無視。Collabora 到着後は
          // クロスオリジンで参照が throw する＝読み込み完了。
          try {
            if (iframeRef.current?.contentWindow?.location.href === "about:blank") return;
          } catch {
            /* クロスオリジン＝Collabora の応答が描画された */
          }
          setFrameReady(true);
        }}
      />
    </div>
  );
});
