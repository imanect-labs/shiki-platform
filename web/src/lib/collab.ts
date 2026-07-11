/// ノート共同編集の WebSocket プロバイダ（Task 11P.3・y-websocket 互換ワイヤ）。
///
/// サーバ（/collab/docs/{id}/ws・crates/collab）と y-protocols の sync/awareness
/// メッセージを交換する最小プロバイダ。y-websocket パッケージ本体は使わず、
/// BFF セッション Cookie（同一オリジン /api 経由）で認証する。
/// 切断時は指数バックオフで自動再接続し、再接続時の sync step1/2 で全差分を回復する
/// （サーバは Lagged 時に切断する設計のため、この回復経路が正）。

import * as decoding from "lib0/decoding";
import * as encoding from "lib0/encoding";
import {
  applyAwarenessUpdate,
  Awareness,
  encodeAwarenessUpdate,
  removeAwarenessStates,
} from "y-protocols/awareness";
import * as syncProtocol from "y-protocols/sync";
import type * as Y from "yjs";

/// y-protocols のトップレベルメッセージ種別。
const MSG_SYNC = 0;
const MSG_AWARENESS = 1;
const MSG_QUERY_AWARENESS = 3;

export type CollabStatus = "connecting" | "connected" | "disconnected";

export class CollabProvider {
  readonly doc: Y.Doc;
  readonly awareness: Awareness;
  private readonly nodeId: string;
  private ws: WebSocket | null = null;
  private destroyed = false;
  private retries = 0;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private statusListeners = new Set<(s: CollabStatus) => void>();
  /// 初回 sync 完了（step2 受信）リスナー。ロード表示の解除に使う。
  private syncedListeners = new Set<() => void>();
  private hasSynced = false;

  constructor(nodeId: string, doc: Y.Doc) {
    this.nodeId = nodeId;
    this.doc = doc;
    this.awareness = new Awareness(doc);
    this.doc.on("update", this.handleDocUpdate);
    this.awareness.on("update", this.handleAwarenessUpdate);
    this.connect();
  }

  onStatus(listener: (s: CollabStatus) => void): () => void {
    this.statusListeners.add(listener);
    return () => this.statusListeners.delete(listener);
  }

  onSynced(listener: () => void): () => void {
    if (this.hasSynced) listener();
    this.syncedListeners.add(listener);
    return () => this.syncedListeners.delete(listener);
  }

  get synced(): boolean {
    return this.hasSynced;
  }

  destroy(): void {
    this.destroyed = true;
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.doc.off("update", this.handleDocUpdate);
    this.awareness.off("update", this.handleAwarenessUpdate);
    // 自分のプレゼンスを消してから閉じる（他参加者のカーソル残留を防ぐ）。
    removeAwarenessStates(this.awareness, [this.doc.clientID], "destroy");
    this.ws?.close();
    this.ws = null;
  }

  private emitStatus(status: CollabStatus): void {
    for (const listener of this.statusListeners) listener(status);
  }

  private url(): string {
    const proto = window.location.protocol === "https:" ? "wss" : "ws";
    // 既定は BFF 同一オリジン（/api → shiki-server の rewrite が WS も中継する）。
    // 中継できない構成向けに NEXT_PUBLIC_COLLAB_WS_ORIGIN で直結先を上書きできる。
    const override = process.env.NEXT_PUBLIC_COLLAB_WS_ORIGIN;
    if (override) {
      const base = override.replace(/^http/, "ws").replace(/\/$/, "");
      return `${base}/collab/docs/${this.nodeId}/ws`;
    }
    return `${proto}://${window.location.host}/api/collab/docs/${this.nodeId}/ws`;
  }

  private connect(): void {
    if (this.destroyed) return;
    this.emitStatus("connecting");
    const ws = new WebSocket(this.url());
    ws.binaryType = "arraybuffer";
    this.ws = ws;

    ws.onopen = () => {
      this.retries = 0;
      this.emitStatus("connected");
      // sync step1（自分の state vector）→ サーバが step2 を返す。
      const enc = encoding.createEncoder();
      encoding.writeVarUint(enc, MSG_SYNC);
      syncProtocol.writeSyncStep1(enc, this.doc);
      ws.send(encoding.toUint8Array(enc));
      // 自分のプレゼンスを通知し、他参加者の状態も要求する。
      const state = this.awareness.getLocalState();
      if (state !== null) {
        const awarenessEnc = encoding.createEncoder();
        encoding.writeVarUint(awarenessEnc, MSG_AWARENESS);
        encoding.writeVarUint8Array(
          awarenessEnc,
          encodeAwarenessUpdate(this.awareness, [this.doc.clientID]),
        );
        ws.send(encoding.toUint8Array(awarenessEnc));
      }
      const query = encoding.createEncoder();
      encoding.writeVarUint(query, MSG_QUERY_AWARENESS);
      ws.send(encoding.toUint8Array(query));
    };

    ws.onmessage = (event: MessageEvent<ArrayBuffer>) => {
      const dec = decoding.createDecoder(new Uint8Array(event.data));
      const kind = decoding.readVarUint(dec);
      if (kind === MSG_SYNC) {
        const enc = encoding.createEncoder();
        encoding.writeVarUint(enc, MSG_SYNC);
        const messageType = syncProtocol.readSyncMessage(dec, enc, this.doc, this);
        // step1 への応答（step2）だけ送り返す。update への応答は無い。
        if (encoding.length(enc) > 1) {
          ws.send(encoding.toUint8Array(enc));
        }
        if (messageType === syncProtocol.messageYjsSyncStep2 && !this.hasSynced) {
          this.hasSynced = true;
          for (const listener of this.syncedListeners) listener();
        }
      } else if (kind === MSG_AWARENESS) {
        applyAwarenessUpdate(this.awareness, decoding.readVarUint8Array(dec), this);
      }
    };

    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.ws = null;
      this.emitStatus("disconnected");
      this.scheduleReconnect();
    };
    ws.onerror = () => {
      // onclose が続いて呼ばれる（再接続はそちらで手配）。
    };
  }

  private scheduleReconnect(): void {
    if (this.destroyed) return;
    // 1s → 2s → 4s → … 最大 30s の指数バックオフ。権限剥奪による切断（4403）でも
    // 再接続は試みるが、サーバ側で再び拒否される（fail-closed はサーバの責務）。
    const delay = Math.min(1000 * 2 ** this.retries, 30_000);
    this.retries += 1;
    this.reconnectTimer = setTimeout(() => this.connect(), delay);
  }

  private handleDocUpdate = (update: Uint8Array, origin: unknown): void => {
    // 自分がサーバから受けた update の再送信を防ぐ（origin=this はサーバ由来）。
    if (origin === this || this.ws?.readyState !== WebSocket.OPEN) return;
    const enc = encoding.createEncoder();
    encoding.writeVarUint(enc, MSG_SYNC);
    syncProtocol.writeUpdate(enc, update);
    this.ws.send(encoding.toUint8Array(enc));
  };

  private handleAwarenessUpdate = (
    {
      added,
      updated,
      removed,
    }: { added: number[]; updated: number[]; removed: number[] },
    origin: unknown,
  ): void => {
    if (origin === this || this.ws?.readyState !== WebSocket.OPEN) return;
    const changed = [...added, ...updated, ...removed];
    const enc = encoding.createEncoder();
    encoding.writeVarUint(enc, MSG_AWARENESS);
    encoding.writeVarUint8Array(enc, encodeAwarenessUpdate(this.awareness, changed));
    this.ws.send(encoding.toUint8Array(enc));
  };
}
