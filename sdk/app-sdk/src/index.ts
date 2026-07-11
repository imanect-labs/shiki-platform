/// shiki ミニアプリ SDK（Task 9.14）。
///
/// 公開 API ゲートウェイ（第2リスナ・別オリジン）を叩く薄い型付きクライアント。B1（ブラウザ）
/// では **ホスト支援 PKCE** でシェルからトークンを受け取り、B2（サーバ関数）では `Shiki.*`
/// ホスト関数がゲートウェイを代理するため SDK 直呼びは不要（この SDK は主に B1 と外部連携向け）。
///
/// 型はゲートウェイの DTO と一致させる（手書きミラーは薄いラッパのみ・codegen 拡張は
/// gen-api で app-gateway ApiDoc を足す後続作業に委ねる）。

/// ゲートウェイのベース URL とトークン供給。
export interface GatewayConfig {
  /// ゲートウェイ origin（例 `https://gw.example`）。
  baseUrl: string;
  /// アクセストークン供給（B1 はシェルの postMessage 由来・期限切れは再取得を返す）。
  getToken: () => Promise<string> | string;
}

export interface GwTable {
  id: string;
  name: string;
  schema_version: number;
  updated_at: string;
}

export interface GwRecordList<T = Record<string, unknown>> {
  items: { id: string; table_id: string; data: T; rev: number; owner: string }[];
  shares_truncated: boolean;
}

export interface GwRecord<T = Record<string, unknown>> {
  id: string;
  table_id: string;
  data: T;
  rev: number;
  owner: string;
}

export class GatewayError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
    this.name = "GatewayError";
  }
}

/// 型付きゲートウェイクライアント。
export class ShikiGateway {
  constructor(private readonly config: GatewayConfig) {}

  private async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const token = await this.config.getToken();
    const res = await fetch(`${this.config.baseUrl}${path}`, {
      method,
      headers: {
        authorization: `Bearer ${token}`,
        ...(body !== undefined ? { "content-type": "application/json" } : {}),
      },
      body: body !== undefined ? JSON.stringify(body) : undefined,
    });
    if (!res.ok) {
      let message = `HTTP ${res.status}`;
      try {
        const b = (await res.json()) as { error?: string };
        message = b.error ?? message;
      } catch {
        // 本文なし。
      }
      throw new GatewayError(message, res.status);
    }
    return (res.status === 204 ? undefined : await res.json()) as T;
  }

  // --- data.* ---
  listTables(): Promise<GwTable[]> {
    return this.request("GET", "/gw/data/tables");
  }
  listRecords<T = Record<string, unknown>>(
    tableId: string,
    opts?: { limit?: number; offset?: number },
  ): Promise<GwRecordList<T>> {
    const q = new URLSearchParams();
    if (opts?.limit !== undefined) q.set("limit", String(opts.limit));
    if (opts?.offset !== undefined) q.set("offset", String(opts.offset));
    const qs = q.toString();
    return this.request("GET", `/gw/data/tables/${tableId}/records${qs ? `?${qs}` : ""}`);
  }
  getRecord<T = Record<string, unknown>>(tableId: string, recordId: string): Promise<GwRecord<T>> {
    return this.request("GET", `/gw/data/tables/${tableId}/records/${recordId}`);
  }
  createRecord<T = Record<string, unknown>>(tableId: string, data: T): Promise<GwRecord<T>> {
    return this.request("POST", `/gw/data/tables/${tableId}/records`, { data });
  }
  updateRecord<T = Record<string, unknown>>(
    tableId: string,
    recordId: string,
    patch: Partial<T>,
    expectedRev: number,
  ): Promise<GwRecord<T>> {
    return this.request("PATCH", `/gw/data/tables/${tableId}/records/${recordId}`, {
      patch,
      expected_rev: expectedRev,
    });
  }
  query<R = unknown>(tableId: string, dataQuery: unknown): Promise<R> {
    return this.request("POST", `/gw/data/tables/${tableId}/query`, dataQuery);
  }
  transition<T = Record<string, unknown>>(
    tableId: string,
    recordId: string,
    to: string,
    expectedRev: number,
  ): Promise<GwRecord<T>> {
    return this.request("POST", `/gw/data/tables/${tableId}/records/${recordId}/transition`, {
      to,
      expected_rev: expectedRev,
    });
  }

  // --- identity / notify / rag ---
  whoami(): Promise<{ user_sub: string; app_id: string; granted_scopes: string[] }> {
    return this.request("GET", "/gw/whoami");
  }
  identity(): Promise<{ user_sub: string; tenant: string; roles: string[] }> {
    return this.request("GET", "/gw/identity/me");
  }
  ragQuery(query: string, topK?: number): Promise<{ hits: unknown[] }> {
    return this.request("POST", "/gw/rag/query", { query, top_k: topK });
  }
  notify(recipient: string, title: string, body?: string): Promise<{ id: string }> {
    return this.request("POST", "/gw/notify/send", { recipient, title, body });
  }

  /// SSE を購読する（events / ai.*）。`onEvent(type, data)` を各フレームで呼ぶ。
  async subscribe(
    path: string,
    onEvent: (type: string, data: unknown) => void,
    signal?: AbortSignal,
  ): Promise<void> {
    const token = await this.config.getToken();
    const res = await fetch(`${this.config.baseUrl}${path}`, {
      headers: { authorization: `Bearer ${token}` },
      signal,
    });
    if (!res.ok || !res.body) throw new GatewayError(`SSE HTTP ${res.status}`, res.status);
    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      const frames = buffer.split("\n\n");
      buffer = frames.pop() ?? "";
      for (const frame of frames) {
        let event = "message";
        let data = "";
        for (const line of frame.split("\n")) {
          if (line.startsWith("event:")) event = line.slice(6).trim();
          else if (line.startsWith("data:")) data += line.slice(5).trim();
        }
        if (data) {
          try {
            onEvent(event, JSON.parse(data));
          } catch {
            onEvent(event, data);
          }
        }
      }
    }
  }
}

/// B1 ホスト支援 PKCE のトークン供給を作る（iframe 側 SDK 利用者向け）。
///
/// シェルへ `shiki:token-request` を postMessage し、`shiki:token` を待つ。SDK 利用者は
/// `new ShikiGateway({ baseUrl, getToken: hostAssistedToken(scopes) })` として使う。
export function hostAssistedToken(scopes: string[]): () => Promise<string> {
  return () =>
    new Promise<string>((resolve, reject) => {
      function onMessage(ev: MessageEvent) {
        const data = ev.data as { type?: string; accessToken?: string; error?: string };
        if (data?.type === "shiki:token" && data.accessToken) {
          window.removeEventListener("message", onMessage);
          resolve(data.accessToken);
        } else if (data?.type === "shiki:token-error") {
          window.removeEventListener("message", onMessage);
          reject(new Error(data.error ?? "token error"));
        }
      }
      window.addEventListener("message", onMessage);
      window.parent.postMessage({ type: "shiki:token-request", scopes }, "*");
    });
}
