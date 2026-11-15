/// 職員間メッセージ（Phase 14・design §4.14）の API。
///
/// 型は OpenAPI(utoipa) から生成したものだけを使う（手書きの対応物を作らない）。認可・監査はすべて
/// shiki-server 側（`crates/messaging`）が持ち、ここは呼び出しと表示用の小さな変換だけを置く。

import type { components } from "@/generated/api";
import { apiFetch, apiUrl } from "@/lib/api";

type S = components["schemas"];
export type ChannelView = S["ChannelView"];
export type ChannelKind = S["ChannelKind"];
export type NotifyLevel = S["NotifyLevel"];
export type MemberView = S["MemberView"];
export type MessageView = S["MessageView"];
export type AuthorView = S["AuthorView"];
export type FileView = S["FileView"];
export type ContentBlock = S["ContentBlock"];
export type Mention = S["Mention"];
export type NewMessage = S["NewMessage"];
export type CreateChannel = S["CreateChannel"];
export type SearchHit = S["SearchHit"];
export type MentionNotice = S["MentionNotice"];
export type AgentReplyView = S["AgentReplyView"];
export type AgentProfileView = S["AgentProfileView"];
export type PublishResult = S["PublishResult"];
export type MessagingSettings = S["MessagingSettings"];
export type AccessGrant = S["AccessGrant"];
export type AccessRequest = S["AccessRequest"];
export type AdminChannel = S["AdminChannel"];
export type AdminMessage = S["AdminMessage"];
export type DirectoryUser = S["DirectoryUserResponse"];
export type DirectoryRole = S["DirectoryRoleResponse"];

/// 本文で使えるブロック（text / file_ref / mention の部分集合・サーバの `validate_blocks`）。
export type MessageBlock = Extract<ContentBlock, { type: "text" | "file_ref" | "mention" }>;

/// 本文の上限（サーバの `MAX_TEXT_CHARS` / `MAX_ATTACHMENTS` と同じ・正はサーバの検査）。
export const MAX_TEXT_CHARS = 4000;
export const MAX_ATTACHMENTS = 10;
/// パーソナルエージェントの既定の名前（サーバの `DEFAULT_AGENT_NAME`）。
export const DEFAULT_AGENT_NAME = "シキ";

/// 状態コードと、画面にそのまま出せる文言を持つ誤り。
export class MessagingError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "MessagingError";
  }
}

function fallbackMessage(status: number): string {
  switch (status) {
    case 400:
      return "入力を受け付けられませんでした";
    case 401:
      return "ログインし直してください";
    case 403:
      return "この操作の権限がありません";
    case 404:
      return "見つかりません（削除されたか、参加していません）";
    case 409:
      return "他の操作と競合しました。読み込み直してください";
    case 503:
      return "メッセージは一時的に利用できません";
    default:
      return `エラーが発生しました（${status}）`;
  }
}

async function fail(res: Response): Promise<never> {
  let message = "";
  try {
    const body = (await res.json()) as { message?: string; detail?: string };
    message = body.message ?? body.detail ?? "";
  } catch {
    // 本文が JSON でない（プロキシのエラーページ等）。
  }
  throw new MessagingError(res.status, message || fallbackMessage(res.status));
}

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) return fail(res);
  return (await res.json()) as T;
}

async function empty(res: Response): Promise<void> {
  if (!res.ok) await fail(res);
}

const send = (method: string, body?: unknown): RequestInit => ({
  method,
  headers: body === undefined ? undefined : { "Content-Type": "application/json" },
  body: body === undefined ? undefined : JSON.stringify(body),
});

const enc = encodeURIComponent;

function qs(params: Record<string, string | number | boolean | undefined | null>): string {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v === undefined || v === null || v === "") continue;
    q.set(k, String(v));
  }
  const s = q.toString();
  return s ? `?${s}` : "";
}

// ---- チャンネル ----

export async function listChannels(): Promise<ChannelView[]> {
  return json(await apiFetch("/messaging/channels"));
}

export async function browseChannels(q: string, archived = false): Promise<ChannelView[]> {
  return json(await apiFetch(`/messaging/channels/browse${qs({ q, archived: archived || undefined })}`));
}

export async function getChannel(id: string): Promise<ChannelView> {
  return json(await apiFetch(`/messaging/channels/${enc(id)}`));
}

export async function createChannel(input: CreateChannel): Promise<ChannelView> {
  return json(await apiFetch("/messaging/channels", send("POST", input)));
}

export async function updateChannel(id: string, patch: { name?: string; topic?: string }): Promise<ChannelView> {
  return json(await apiFetch(`/messaging/channels/${enc(id)}`, send("PATCH", patch)));
}

export async function setArchived(id: string, archived: boolean): Promise<ChannelView> {
  return json(await apiFetch(`/messaging/channels/${enc(id)}/archive`, send("PUT", { archived })));
}

export async function joinChannel(id: string): Promise<ChannelView> {
  return json(await apiFetch(`/messaging/channels/${enc(id)}/join`, send("POST")));
}

export async function listMembers(id: string): Promise<MemberView[]> {
  return json(await apiFetch(`/messaging/channels/${enc(id)}/members`));
}

export async function inviteMembers(id: string, userIds: string[]): Promise<MemberView[]> {
  return json(await apiFetch(`/messaging/channels/${enc(id)}/members`, send("POST", { user_ids: userIds })));
}

/// 退出（自分）・外す（管理者）。
export async function removeMember(id: string, userId: string): Promise<void> {
  return empty(await apiFetch(`/messaging/channels/${enc(id)}/members/${enc(userId)}`, send("DELETE")));
}

export async function setMemberRole(
  id: string,
  userId: string,
  role: { admin?: boolean; poster?: boolean },
): Promise<MemberView[]> {
  return json(await apiFetch(`/messaging/channels/${enc(id)}/members/${enc(userId)}`, send("PATCH", role)));
}

export async function setNotify(id: string, level: NotifyLevel): Promise<void> {
  return empty(await apiFetch(`/messaging/channels/${enc(id)}/notify`, send("PUT", { level })));
}

export async function hideDm(id: string): Promise<void> {
  return empty(await apiFetch(`/messaging/channels/${enc(id)}/hide`, send("POST")));
}

export async function markRead(id: string, at?: string): Promise<void> {
  return empty(await apiFetch(`/messaging/channels/${enc(id)}/read`, send("POST", at ? { at } : {})));
}

export async function openDm(userId: string): Promise<ChannelView> {
  return json(await apiFetch("/messaging/dms", send("POST", { user_id: userId })));
}

// ---- 発言 ----

/// 最上位の発言（新しい順・`before` より前）。
export async function listMessages(id: string, before?: string, limit = 50): Promise<MessageView[]> {
  return json(await apiFetch(`/messaging/channels/${enc(id)}/messages${qs({ before, limit })}`));
}

export async function postMessage(id: string, input: NewMessage): Promise<MessageView> {
  return json(await apiFetch(`/messaging/channels/${enc(id)}/messages`, send("POST", input)));
}

/// スレッド（親＋返信・古い順）。
export async function getThread(id: string, parentId: string): Promise<MessageView[]> {
  return json(await apiFetch(`/messaging/channels/${enc(id)}/threads/${enc(parentId)}`));
}

export async function getMessage(id: string): Promise<MessageView> {
  return json(await apiFetch(`/messaging/messages/${enc(id)}`));
}

export async function editMessage(id: string, blocks: MessageBlock[]): Promise<MessageView> {
  return json(await apiFetch(`/messaging/messages/${enc(id)}`, send("PATCH", { blocks })));
}

export async function deleteMessage(id: string): Promise<void> {
  return empty(await apiFetch(`/messaging/messages/${enc(id)}`, send("DELETE")));
}

export async function react(id: string, emoji: string, on: boolean): Promise<MessageView> {
  return json(await apiFetch(`/messaging/messages/${enc(id)}/reactions`, send("POST", { emoji, on })));
}

export async function searchMessages(q: string, limit = 30): Promise<SearchHit[]> {
  return json(await apiFetch(`/messaging/search${qs({ q, limit })}`));
}

export async function listMentions(unread = false): Promise<MentionNotice[]> {
  return json(await apiFetch(`/messaging/mentions${qs({ unread: unread || undefined })}`));
}

/// メンションを既読にする（空なら全部）。
export async function readMentions(messageIds: string[] = []): Promise<void> {
  return empty(await apiFetch("/messaging/mentions/read", send("POST", { message_ids: messageIds })));
}

// ---- パーソナルエージェント ----

export async function getAgentProfile(): Promise<AgentProfileView> {
  return json(await apiFetch("/messaging/agent"));
}

/// 名前を変える（空文字で既定の「シキ」に戻す）。
export async function setAgentName(name: string): Promise<AgentProfileView> {
  return json(await apiFetch("/messaging/agent", send("PUT", { name })));
}

/// アイコンを置く（PNG・JPEG・WebP・256 KB まで。形式はサーバが中身で判定する）。
export async function setAgentIcon(file: Blob): Promise<AgentProfileView> {
  return json(
    await apiFetch("/messaging/agent/icon", {
      method: "PUT",
      headers: { "Content-Type": file.type || "application/octet-stream" },
      body: file,
    }),
  );
}

export async function deleteAgentIcon(): Promise<AgentProfileView> {
  return json(await apiFetch("/messaging/agent/icon", send("DELETE")));
}

/// エージェントのアイコンの URL（`<img src>` 用）。`v` は差し替え後にキャッシュを外すための値。
export function agentIconUrl(userId: string, v?: string | number): string {
  return apiUrl(`/messaging/agents/${enc(userId)}/icon${v !== undefined ? `?v=${enc(String(v))}` : ""}`);
}

export async function listAgentReplies(channelId: string): Promise<AgentReplyView[]> {
  return json(await apiFetch(`/messaging/channels/${enc(channelId)}/agent-replies`));
}

export async function publishAgentReply(id: string, confirmed: boolean): Promise<PublishResult> {
  return json(await apiFetch(`/messaging/agent-replies/${enc(id)}/publish`, send("POST", { confirmed })));
}

export async function dismissAgentReply(id: string): Promise<void> {
  return empty(await apiFetch(`/messaging/agent-replies/${enc(id)}`, send("DELETE")));
}

// ---- 組織の設定・監査担当者・承認 ----

export async function getMessagingSettings(): Promise<MessagingSettings> {
  return json(await apiFetch("/org-admin/messaging/settings"));
}

export async function putMessagingSettings(body: MessagingSettings): Promise<MessagingSettings> {
  return json(await apiFetch("/org-admin/messaging/settings", send("PUT", body)));
}

export async function auditChannels(q: string): Promise<AdminChannel[]> {
  return json(await apiFetch(`/org-audit/messaging/channels${qs({ q })}`));
}

export async function auditMessages(id: string, from: string, until: string): Promise<AdminMessage[]> {
  return json(await apiFetch(`/org-audit/messaging/channels/${enc(id)}/messages${qs({ from, until })}`));
}

/// エクスポートの URL（JSON のダウンロード・ドライブには保存しない）。
export function auditExportUrl(id: string, from: string, until: string): string {
  return apiUrl(`/org-audit/messaging/channels/${enc(id)}/export${qs({ from, until })}`);
}

export async function requestAccess(body: AccessRequest): Promise<AccessGrant> {
  return json(await apiFetch("/org-audit/messaging/access-requests", send("POST", body)));
}

/// 閲覧の申請（承認者は全件・監査担当者は自分の申請）。どちらでもなければ 403。
export async function listAccessRequests(): Promise<AccessGrant[]> {
  return json(await apiFetch("/messaging/access-requests"));
}

export async function decideAccess(id: string, approve: boolean): Promise<AccessGrant> {
  return json(await apiFetch(`/messaging/access-requests/${enc(id)}/decision`, send("POST", { approve })));
}

export async function revokeAccess(id: string): Promise<AccessGrant> {
  return json(await apiFetch(`/messaging/access-requests/${enc(id)}/revoke`, send("POST")));
}

// ---- ディレクトリ（招待・DM の相手・メンションのロール） ----

export async function searchUsers(q: string, limit = 20): Promise<DirectoryUser[]> {
  const r = await json<S["DirectorySearchResponse"]>(await apiFetch(`/directory/users${qs({ q, limit })}`));
  return r.items;
}

export async function searchRoles(q: string, limit = 10): Promise<DirectoryRole[]> {
  const r = await json<S["DirectoryRoleSearchResponse"]>(await apiFetch(`/directory/roles${qs({ q, limit })}`));
  return r.items;
}

// ---- 表示用の変換 ----

/// 書き手の表示名（無効化された利用者は「（無効）」を付ける）。
export function authorName(a: AuthorView): string {
  return a.active ? a.display_name : `${a.display_name}（無効）`;
}

/// メンションの表記（`@名前`）。`names` は利用者 id・ロール id → 表示名。
export function mentionLabel(m: Mention, names: ReadonlyMap<string, string>, agentName: string): string {
  switch (m.kind) {
    case "user":
      return `@${names.get(m.user_id) ?? "利用者"}`;
    case "role":
      return `@${names.get(`role:${m.role_id}`) ?? m.role_id}`;
    case "channel":
      return "@channel";
    case "agent":
      return `@${agentName}`;
  }
}

/// 発言を平文に落とす（抜粋・検索結果用）。読めない添付は名前を出さない。
export function plainText(msg: MessageView, names: ReadonlyMap<string, string>, agentName: string): string {
  return msg.blocks
    .map((b, i) => {
      if (b.type === "text") return b.text;
      if (b.type === "mention") return mentionLabel(b.mention, names, agentName);
      if (b.type === "file_ref") {
        const f = msg.files.find((x) => x.block_index === i);
        return f?.available && f.name ? `［${f.name}］` : "［参照できない添付］";
      }
      return "";
    })
    .join("")
    .replace(/\s+/g, " ")
    .trim();
}

/// 発言の中の自分のエージェントへのメンションの有無（呼び出しの合図）。
export function mentionsAgent(blocks: readonly ContentBlock[]): boolean {
  return blocks.some((b) => b.type === "mention" && b.mention.kind === "agent");
}

/// SSE の購読（`GET /messaging/channels/{id}/stream`）。
export type ChannelStreamHandlers = {
  onMessage: (m: MessageView) => void;
  onAgentReply: () => void;
  onChannel: () => void;
  /// 読めなくなった（退出・外された・削除）。購読は閉じている。
  onLeave: () => void;
  /// 繋がった（最初と繋ぎ直しのたび）。取りこぼしは `since` で拾い直す。
  onOpen?: () => void;
  /// 切れた（繋ぎ直し待ち）。
  onError?: () => void;
};

/// サーバの keepalive（15 秒ごと）がこの間届かなければ、黙って死んだ接続とみなす。
const STREAM_IDLE_MS = 45_000;

/// チャンネルを購読する。切れたら指数バックオフで繋ぎ直し、最後に受け取った発言の時刻から再開する
/// （サーバは `Last-Event-ID` と同じ `since` を受け付ける）。返り値を呼ぶと閉じる。
export function subscribeChannel(channelId: string, h: ChannelStreamHandlers): () => void {
  let es: EventSource | null = null;
  let closed = false;
  let since: string | null = null;
  let attempt = 0;
  let retry: ReturnType<typeof setTimeout> | null = null;
  let lastActivity = Date.now();
  const watchdog = setInterval(() => {
    if (closed || !es || es.readyState !== EventSource.OPEN) return;
    if (Date.now() - lastActivity > STREAM_IDLE_MS) reconnect();
  }, 5_000);

  const parse = (ev: MessageEvent): MessageView | null => {
    try {
      return JSON.parse(String(ev.data)) as MessageView;
    } catch {
      return null;
    }
  };

  const close = () => {
    closed = true;
    clearInterval(watchdog);
    if (retry) clearTimeout(retry);
    es?.close();
    es = null;
  };

  function reconnect() {
    es?.close();
    es = null;
    if (closed) return;
    h.onError?.();
    const delay = Math.min(30_000, 1_000 * 2 ** attempt) + Math.floor(Math.random() * 500);
    attempt += 1;
    if (retry) clearTimeout(retry);
    retry = setTimeout(connect, delay);
  }

  function connect() {
    if (closed) return;
    const q = since ? `?since=${enc(since)}` : "";
    const src = new EventSource(apiUrl(`/messaging/channels/${enc(channelId)}/stream${q}`), {
      withCredentials: true,
    });
    es = src;
    const touch = () => {
      lastActivity = Date.now();
    };
    src.addEventListener("ready", () => {
      attempt = 0;
      touch();
      h.onOpen?.();
    });
    src.addEventListener("keepalive", touch);
    src.addEventListener("message", (ev) => {
      touch();
      const e = ev as MessageEvent;
      if (e.lastEventId) since = e.lastEventId;
      const m = parse(e);
      if (m) h.onMessage(m);
    });
    src.addEventListener("agent_reply", () => {
      touch();
      h.onAgentReply();
    });
    src.addEventListener("channel", () => {
      touch();
      h.onChannel();
    });
    src.addEventListener("leave", () => {
      close();
      h.onLeave();
    });
    src.onerror = () => {
      if (closed || es !== src) return;
      if (src.readyState === EventSource.CLOSED) {
        // HTTP の誤り（404・403）で閉じた。EventSource は状態コードを読めないので REST で確かめ、
        // 読めなくなっていたら繋ぎ直さずに知らせる（読めない購読を叩き続けない）。
        src.close();
        es = null;
        getChannel(channelId).then(
          () => reconnect(),
          (e: unknown) => {
            if (closed) return;
            if (e instanceof MessagingError && (e.status === 404 || e.status === 403)) {
              close();
              h.onLeave();
            } else reconnect();
          },
        );
        return;
      }
      // 自前で繋ぎ直す（`since` を付け直すため・ブラウザの自動再接続はクエリを変えられない）。
      reconnect();
    };
  }

  // 購読の開始時刻（それより後の変更を受け取る）。サーバの時刻とずれても取りこぼさないよう少し戻す。
  since = new Date(Date.now() - 5_000).toISOString();
  connect();
  return close;
}
