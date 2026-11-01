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
  /** 次ページのカーソル（無ければ終端）。 */
  next_cursor?: string;
}

export interface GwRecord<T = Record<string, unknown>> {
  id: string;
  table_id: string;
  data: T;
  rev: number;
  owner: string;
}

/// ゲートウェイが断った呼び出し。`message` はゲートウェイの文言（そのまま利用者に見せてよい日本語）。
///
/// `code` は理由を分けて扱いたい誤りにだけ付く（無ければ `undefined`）。AI（`invokeLlm`・`invokeAgent`）でモデルを
/// 使えないときは、ワークフローの誤りと同じ語彙の次の 3 つ:
/// - `model_choice_disabled`: 組織がモデルの選択を禁じていて、既定と違うモデルを指定した（モデルを指定しなければ動く）
/// - `model_unavailable`: 指定したモデルはこの組織では使えない
/// - `no_model`: 組織で使えるモデルが無い
export class GatewayError extends Error {
  readonly status: number;
  readonly code: string | undefined;

  constructor(message: string, status: number, code?: string) {
    super(message);
    this.name = "GatewayError";
    this.status = status;
    this.code = code;
  }
}

/// 失敗した応答を [`GatewayError`] にする（本文の `error` を文言、`code` を理由として読む）。
async function errorOf(res: Response): Promise<GatewayError> {
  let message = `HTTP ${res.status}`;
  let code: string | undefined;
  try {
    const b = (await res.json()) as { error?: unknown; code?: unknown };
    if (typeof b.error === "string" && b.error) message = b.error;
    if (typeof b.code === "string" && b.code) code = b.code;
  } catch {
    // 本文なし・JSON でない。
  }
  return new GatewayError(message, res.status, code);
}

/// AI の 1 回の呼び出し（`/gw/ai/llm/invoke`）の要求。形はゲートウェイの要求のとおりで、ここでは SDK が説明に使う項目
/// だけを型にする（手書きの写しを増やさない。ほかの項目もそのまま送る）。
///
/// `model` を省略すると組織の既定のモデルで動く。組織がモデルの選択を禁じているときに既定と違うモデルを書くと、
/// `model_choice_disabled` で断られる。思考の深さ（`effort`）は、組織がモデルの選択を禁じているときと、省略して
/// 組織の既定のおまかせ（Auto）で動くときは使われない。
export type GwLlmInvokeRequest = { messages: unknown[]; model?: string } & Record<string, unknown>;

/// AI エージェントの呼び出し（`/gw/ai/agent/invoke`）の要求。`model` の扱いは [`GwLlmInvokeRequest`] と同じ。
export type GwAgentInvokeRequest = { prompt: string; model?: string } & Record<string, unknown>;

/// 型付きゲートウェイクライアント。
export class ShikiGateway {
  private readonly config: GatewayConfig;

  constructor(config: GatewayConfig) {
    this.config = config;
  }

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
    if (!res.ok) throw await errorOf(res);
    return (res.status === 204 ? undefined : await res.json()) as T;
  }

  // --- data.* ---
  listTables(): Promise<GwTable[]> {
    return this.request("GET", "/gw/data/tables");
  }
  listRecords<T = Record<string, unknown>>(
    tableId: string,
    opts?: { limit?: number; cursor?: string },
  ): Promise<GwRecordList<T>> {
    const q = new URLSearchParams();
    if (opts?.limit !== undefined) q.set("limit", String(opts.limit));
    // keyset ページング（前ページの next_cursor を渡す・OFFSET は廃止）。
    if (opts?.cursor !== undefined) q.set("cursor", opts.cursor);
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

  /// SSE を購読する（events）。`onEvent(type, data)` を各フレームで呼ぶ。断られたら [`GatewayError`] を投げる。
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
    await readSse(res, onEvent);
  }

  /// AI に 1 回聞く（SSE）。文字は `text` のイベントで少しずつ届き、`done` で終わる。モデルを使えないときなどは、
  /// 流れ始める前に [`GatewayError`] を投げる（理由は `code`）。
  invokeLlm(
    request: GwLlmInvokeRequest,
    onEvent: (type: string, data: unknown) => void,
    signal?: AbortSignal,
  ): Promise<void> {
    return this.postStream("/gw/ai/llm/invoke", request, onEvent, signal);
  }

  /// AI エージェントに頼む（SSE）。誤りの扱いは [`ShikiGateway.invokeLlm`] と同じ。
  invokeAgent(
    request: GwAgentInvokeRequest,
    onEvent: (type: string, data: unknown) => void,
    signal?: AbortSignal,
  ): Promise<void> {
    return this.postStream("/gw/ai/agent/invoke", request, onEvent, signal);
  }

  private async postStream(
    path: string,
    body: unknown,
    onEvent: (type: string, data: unknown) => void,
    signal?: AbortSignal,
  ): Promise<void> {
    const token = await this.config.getToken();
    const res = await fetch(`${this.config.baseUrl}${path}`, {
      method: "POST",
      headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
      body: JSON.stringify(body),
      signal,
    });
    await readSse(res, onEvent);
  }
}

/// SSE の応答を最後まで読み、フレームごとに `onEvent(type, data)` を呼ぶ（失敗の応答は [`GatewayError`]）。
async function readSse(res: Response, onEvent: (type: string, data: unknown) => void): Promise<void> {
  if (!res.ok) throw await errorOf(res);
  if (!res.body) throw new GatewayError(`HTTP ${res.status}`, res.status);
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

/// シェルがホスト支援 PKCE で渡すトークン（`shiki:token` の知らせ）。
export interface HostToken {
  /// ゲートウェイのアクセストークン。
  accessToken: string;
  /// トークンが使える秒数（シェルが渡さなければ 0）。
  expiresIn: number;
  /// ゲートウェイの場所（origin。`GatewayConfig.baseUrl` に渡す）。シェルが渡さなければ `undefined`。
  gatewayOrigin: string | undefined;
}

/// シェルへトークンを 1 回求める（ホスト支援 PKCE）。シェルはポップアップでログインの確認をしてからトークンを返す。
///
/// 答えはシェル（`window.parent`）からの知らせだけを読む。シェルが断ったら（`shiki:token-error`）その文で失敗する。
export function requestHostToken(scopes: string[]): Promise<HostToken> {
  return new Promise<HostToken>((resolve, reject) => {
    function onMessage(ev: MessageEvent) {
      if (ev.source !== window.parent) return;
      const data = ev.data as {
        type?: string;
        accessToken?: unknown;
        expiresIn?: unknown;
        gatewayOrigin?: unknown;
        error?: unknown;
      };
      if (data?.type === "shiki:token" && typeof data.accessToken === "string" && data.accessToken) {
        window.removeEventListener("message", onMessage);
        resolve({
          accessToken: data.accessToken,
          expiresIn: typeof data.expiresIn === "number" ? data.expiresIn : 0,
          gatewayOrigin:
            typeof data.gatewayOrigin === "string" && data.gatewayOrigin ? data.gatewayOrigin : undefined,
        });
      } else if (data?.type === "shiki:token-error") {
        window.removeEventListener("message", onMessage);
        reject(new Error(typeof data.error === "string" && data.error ? data.error : "トークンを受け取れませんでした"));
      }
    }
    window.addEventListener("message", onMessage);
    window.parent.postMessage({ type: "shiki:token-request", scopes }, "*");
  });
}

/// B1 ホスト支援 PKCE のトークン供給を作る（iframe 側 SDK 利用者向け）。
///
/// 呼ぶたびにシェルへ `shiki:token-request` を postMessage し、`shiki:token` を待つ。ゲートウェイの場所も
/// シェルから受け取るなら [`hostAssistedGateway`] を使う（トークンの使い回しもする）。SDK 利用者は
/// `new ShikiGateway({ baseUrl, getToken: hostAssistedToken(scopes) })` として使う。
export function hostAssistedToken(scopes: string[]): () => Promise<string> {
  return () => requestHostToken(scopes).then((t) => t.accessToken);
}

/// トークンを期限のこの秒数前に求め直す（呼び出しの途中で切れないように）。
const TOKEN_RENEW_MARGIN_SECONDS = 30;

/// ホスト支援 PKCE で、シェルが渡すゲートウェイの場所とトークンを使うクライアントを作る（B1 のアプリ向け）。
///
/// 最初にシェルへトークンを求め、その答えのゲートウェイの場所を `baseUrl` にする（アプリが場所を別に知らなくて
/// よい）。トークンは期限の 30 秒前まで使い回し、それを過ぎたらシェルに求め直す（同時に求めた分は 1 回にまとめる）。
/// シェルがゲートウェイの場所を渡さなければ失敗する。
export async function hostAssistedGateway(scopes: string[]): Promise<ShikiGateway> {
  let current = await requestHostToken(scopes);
  const baseUrl = current.gatewayOrigin;
  if (!baseUrl) throw new Error("シェルがゲートウェイの場所を渡しませんでした");
  let expiresAt = Date.now() + current.expiresIn * 1000;
  let pending: Promise<HostToken> | null = null;
  const getToken = async (): Promise<string> => {
    if (Date.now() < expiresAt - TOKEN_RENEW_MARGIN_SECONDS * 1000) return current.accessToken;
    pending ??= requestHostToken(scopes).finally(() => {
      pending = null;
    });
    const next = await pending;
    current = next;
    expiresAt = Date.now() + next.expiresIn * 1000;
    return next.accessToken;
  };
  return new ShikiGateway({ baseUrl, getToken });
}
