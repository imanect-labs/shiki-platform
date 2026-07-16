"use client";

/// GrapesJS 砂箱エディタのホスト（Task 11.2・design §4.8.3）。
///
/// - エディタは apps オリジンの opaque origin iframe（`allow-same-origin` なし・PIT-40 第4層）。
///   認証情報は渡さない。Yjs doc・CollabProvider は本コンポーネント（アプリオリジン）が保持する。
/// - エディタからの `slide:changed` を Yjs へ**単一スプライス差分**で適用（origin=grapes-local）。
/// - リモート（他ユーザー/AI）の変更は origin タグで見分け、表示中スライドなら再ロードする。
/// - バンドル未配備（/builtin 404）は ready が来ないことで検出し、閲覧フォールバックする。

import { Loader2 } from "lucide-react";
import * as React from "react";
import * as Y from "yjs";

import { EditorBridge } from "@/components/slides/editor-bridge";
import { b1Origin } from "@/lib/miniapp-b1-api";
import { LOCAL_ORIGIN, readSlideHtml, updateSlideHtml } from "@/lib/slides-doc";

/// ready 待ちのタイムアウト（未配備検出）。
const READY_TIMEOUT_MS = 8_000;

export function SlideEditorHost({
  doc,
  slideId,
  onUnavailable,
}: {
  doc: Y.Doc;
  /// 編集対象のスライド id（フィルムストリップの選択・null はデッキ空）。
  slideId: string | null;
  /// エディタバンドル未配備時に呼ぶ（親が閲覧フォールバックへ切り替える）。
  onUnavailable: () => void;
}) {
  const iframeRef = React.useRef<HTMLIFrameElement | null>(null);
  const bridgeRef = React.useRef<EditorBridge | null>(null);
  const [ready, setReady] = React.useState(false);
  // エディタに最後に渡した/エディタから最後に受けた HTML（再ロード判定のエコー抑制）。
  const lastHtmlRef = React.useRef<Map<string, string>>(new Map());
  const slideIdRef = React.useRef<string | null>(slideId);
  slideIdRef.current = slideId;

  const loadCurrent = React.useCallback(() => {
    const bridge = bridgeRef.current;
    if (!bridge) return;
    const id = slideIdRef.current;
    if (!id) {
      bridge.send({ type: "deck:empty" });
      return;
    }
    const html = readSlideHtml(doc, id) ?? "";
    lastHtmlRef.current.set(id, html);
    bridge.send({ type: "slide:load", id, html, editable: true });
  }, [doc]);

  // ブリッジの確立（iframe ロード後に port を移譲）。
  // src はリスナ登録**後**にここで設定する — マウント時に src を持たせると、キャッシュ命中や
  // 高速な 404 で load がリスナ登録前に発火し、ブリッジ未確立のまま固まるレースがある。
  React.useEffect(() => {
    const iframe = iframeRef.current;
    if (!iframe) return;
    let bridge: EditorBridge | null = null;
    let readyTimer: number | null = null;
    const onLoad = () => {
      bridge = new EditorBridge(iframe, (msg) => {
        switch (msg.type) {
          case "ready":
            if (readyTimer !== null) window.clearTimeout(readyTimer);
            setReady(true);
            loadCurrent();
            break;
          case "slide:changed": {
            // スライド切替時は「前のスライドの確定分」が選択変更後に届く（正当）。
            // 実在しない id への書き込みは updateSlideHtml が no-op（fail-closed）。
            if (readSlideHtml(doc, msg.id) === null) return;
            lastHtmlRef.current.set(msg.id, msg.html);
            updateSlideHtml(doc, msg.id, msg.html);
            break;
          }
          default:
        }
      });
      bridgeRef.current = bridge;
      readyTimer = window.setTimeout(() => {
        // ready が来ない＝バンドル未配備 or 読み込み失敗（fail-closed で閲覧へ）。
        onUnavailable();
      }, READY_TIMEOUT_MS);
    };
    iframe.addEventListener("load", onLoad);
    iframe.src = `${b1Origin()}/builtin/slide-editor`;
    return () => {
      iframe.removeEventListener("load", onLoad);
      if (readyTimer !== null) window.clearTimeout(readyTimer);
      bridge?.close();
      bridgeRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [doc]);

  // 選択スライドの切替 → エディタへロード。
  React.useEffect(() => {
    if (ready) loadCurrent();
  }, [ready, slideId, loadCurrent]);

  // リモート変更（他ユーザー/AI/インポート）の反映。自分の書き込み（grapes-local）は無視。
  React.useEffect(() => {
    const array = doc.getArray("slides");
    const onDeep = (_events: unknown, txn: Y.Transaction) => {
      if (txn.origin === LOCAL_ORIGIN) return;
      const id = slideIdRef.current;
      if (!id || !ready) return;
      const current = readSlideHtml(doc, id);
      if (current === null) return;
      if (lastHtmlRef.current.get(id) !== current) {
        loadCurrent();
      }
    };
    array.observeDeep(onDeep);
    return () => array.unobserveDeep(onDeep);
  }, [doc, ready, loadCurrent]);

  return (
    <div className="relative h-full min-h-0 w-full overflow-hidden rounded-lg border border-border/60 bg-card/40">
      {!ready ? (
        <div className="absolute inset-0 z-10 flex items-center justify-center gap-2 bg-background/70 text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" aria-hidden />
          エディタを読み込んでいます…
        </div>
      ) : null}
      <iframe
        ref={iframeRef}
        title="スライドエディタ"
        // allow-same-origin は **apps オリジン（アプリ本体とは別オリジン）に対して**であり、
        // アプリの DOM/cookie には同一オリジンポリシーで到達できない。GrapesJS が自身の
        // キャンバス iframe に触るために必要（opaque origin では contentDocument が null）。
        // 通信は配信側 CSP（default-src 'none'）で全遮断（PIT-40 第4層・builtin.rs 参照）。
        sandbox="allow-scripts allow-same-origin"
        className="h-full w-full border-0"
        data-testid="slide-editor-frame"
      />
    </div>
  );
}
