/// 砂箱エディタ ⇄ 親（アプリオリジン）の MessagePort ブリッジ（砂箱側・Task 11.2）。
///
/// 信頼境界: 砂箱は opaque origin で動き、親の DOM/cookie/API に到達できない。
/// 通信は初回 handshake で親から移譲された MessagePort のみ。**親から見て砂箱発の
/// メッセージは敵対的入力**（PIT-23 と同型）— 検証は親側（editor-bridge.ts）が行う。

import type { ExportReport, ExportSlide } from "./export";

/// 親 → 砂箱。
export type HostMessage =
  | { type: "slide:load"; id: string; html: string; editable: boolean }
  | { type: "deck:empty" }
  | { type: "export:run"; slides: ExportSlide[]; title: string };

/// 砂箱 → 親。
export type SandboxMessage =
  | { type: "ready" }
  | { type: "slide:changed"; id: string; html: string }
  | { type: "selection"; id: string; html: string }
  | { type: "selection:clear" }
  | { type: "export:done"; blob: Blob; report: ExportReport }
  | { type: "export:error"; message: string };

/// handshake: 親が `{type:"shiki:editor-port"}` と共に port を postMessage してくる。
export function acceptPort(onMessage: (msg: HostMessage) => void): Promise<MessagePort> {
  return new Promise((resolve) => {
    window.addEventListener("message", function once(ev: MessageEvent) {
      const data = ev.data as { type?: string } | null;
      if (data?.type !== "shiki:editor-port" || !ev.ports[0]) return;
      window.removeEventListener("message", once);
      const port = ev.ports[0];
      port.onmessage = (e: MessageEvent) => onMessage(e.data as HostMessage);
      resolve(port);
    });
  });
}
