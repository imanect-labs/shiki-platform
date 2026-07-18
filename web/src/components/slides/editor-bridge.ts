"use client";

/// 砂箱エディタ ⇄ 親のブリッジ（親側・Task 11.2・PIT-23 と同型の信頼境界）。
///
/// **砂箱発のメッセージは敵対的入力として扱う**: opaque origin の iframe から届く
/// データは型・サイズを検証してからのみ使う（fail-closed・未知メッセージは黙って破棄）。
/// 通信は MessageChannel（handshake で port を iframe へ移譲・第三者へ渡らない）。

/// 親 → 砂箱。
export type HostMessage =
  | { type: "slide:load"; id: string; html: string; editable: boolean }
  | { type: "deck:empty" };

/// 砂箱 → 親（検証済み）。
export type SandboxMessage = { type: "ready" } | { type: "slide:changed"; id: string; html: string };

/// HTML ペイロードの上限（暴走・メモリ圧迫の遮断。1 スライドとして十分大きい）。
const MAX_HTML_BYTES = 1_000_000;

/// 砂箱からの生データを検証する（通らないものは null＝破棄）。
export function parseSandboxMessage(data: unknown): SandboxMessage | null {
  if (typeof data !== "object" || data === null) return null;
  const record = data as Record<string, unknown>;
  switch (record.type) {
    case "ready":
      return { type: "ready" };
    case "slide:changed": {
      const { id, html } = record;
      if (
        typeof id === "string" &&
        id.length > 0 &&
        id.length <= 128 &&
        typeof html === "string" &&
        html.length <= MAX_HTML_BYTES
      ) {
        return { type: "slide:changed", id, html };
      }
      return null;
    }
    default:
      return null;
  }
}

/// iframe へ MessagePort を移譲し、検証済みメッセージを購読するブリッジ。
export class EditorBridge {
  private channel: MessageChannel;
  private closed = false;

  constructor(
    iframe: HTMLIFrameElement,
    private onMessage: (msg: SandboxMessage) => void,
  ) {
    this.channel = new MessageChannel();
    this.channel.port1.onmessage = (ev: MessageEvent) => {
      if (this.closed) return;
      const msg = parseSandboxMessage(ev.data);
      if (msg) this.onMessage(msg);
    };
    // opaque origin のため targetOrigin は "*" だが、**iframe の contentWindow へ直接**
    // port を送るため第三者には渡らない（miniapp-frame.tsx と同じ方針）。
    iframe.contentWindow?.postMessage({ type: "shiki:editor-port" }, "*", [this.channel.port2]);
  }

  send(msg: HostMessage) {
    if (!this.closed) this.channel.port1.postMessage(msg);
  }

  close() {
    this.closed = true;
    this.channel.port1.close();
  }
}
