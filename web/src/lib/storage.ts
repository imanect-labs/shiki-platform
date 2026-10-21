/// Drive（ストレージ）API クライアント。生成型（OpenAPI）を使い手書き型は持たない。
/// 全リストは `next_cursor` を返すカーソルページング（全件取得しない・無限スクロール前提）。
import type { components } from "@/generated/api";

import { apiFetch } from "./api";
import { sha256Hex } from "./sha256";

export type NodeResponse = components["schemas"]["NodeResponse"];
export type ChildrenResponse = components["schemas"]["ChildrenResponse"];
export type CrumbResponse = components["schemas"]["CrumbResponse"];
export type UploadTicketResponse = components["schemas"]["UploadTicketResponse"];
export type DownloadUrlResponse = components["schemas"]["DownloadUrlResponse"];
export type FileVersionResponse = components["schemas"]["FileVersionResponse"];
export type FileVersionsResponse = components["schemas"]["FileVersionsResponse"];
export type DocBlock = components["schemas"]["DocBlock"];
export type DocBlocksPage = components["schemas"]["DocBlocksPage"];
export type ShareEntry = components["schemas"]["ShareEntry"];
export type ShareRole = components["schemas"]["ShareRole"];
export type ShareTarget = components["schemas"]["ShareTarget"];
export type ShareLink = components["schemas"]["ShareLink"];
export type ShareLinkGrant = components["schemas"]["ShareLinkGrant"];
/// リンクの公開範囲（audience）。restricted=既存アクセス者のみ / organization=組織内 / anyone=社内全員。
export type GeneralAccessLevel = components["schemas"]["GeneralAccessLevel"];
export type CreateShareLinkBody = components["schemas"]["CreateShareLinkRequest"];
export type DirectoryUserResponse = components["schemas"]["DirectoryUserResponse"];
export type DirectorySearchResponse = components["schemas"]["DirectorySearchResponse"];
export type DirectoryRoleResponse = components["schemas"]["DirectoryRoleResponse"];
export type DirectoryRoleSearchResponse = components["schemas"]["DirectoryRoleSearchResponse"];

/// 並び替えキー（サーバ側 keyset ソート）。
export type SortField = "name" | "updated" | "size";

/// API エラー（HTTP ステータスを保持。UI のメッセージ分岐に使う）。
export class StorageApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
    this.name = "StorageApiError";
  }
}

async function okJson<T>(res: Response): Promise<T> {
  if (!res.ok) {
    throw new StorageApiError(res.status, await errorMessage(res));
  }
  return (await res.json()) as T;
}

async function okEmpty(res: Response): Promise<void> {
  if (!res.ok) {
    throw new StorageApiError(res.status, await errorMessage(res));
  }
}

async function errorMessage(res: Response): Promise<string> {
  try {
    const body = await res.json();
    if (body && typeof body === "object" && "message" in body) {
      return String((body as { message: unknown }).message);
    }
  } catch {
    /* JSON でなければステータス由来の文言にフォールバック */
  }
  return statusMessage(res.status);
}

/// サーバのエラー本文は `{status,title}`（英語の reason）だけで理由を含まないため、
/// 利用者に読める文言はステータスから組む。「リクエストが失敗しました (409)」のような
/// 番号だけの表示にしない。操作ごとに文脈のある言い方が要る箇所（同名アップロード・
/// フォルダ作成）は呼び出し側が `isConflict` で分岐して上書きする。
function statusMessage(status: number): string {
  switch (status) {
    case 400:
      return "入力内容を受け付けられませんでした。名前や内容を確認してください。";
    case 401:
      return "ログインの有効期限が切れました。再度ログインしてください。";
    case 403:
      return "この操作を行う権限がありません。";
    case 404:
      return "対象が見つかりません。削除または移動された可能性があります。";
    case 409:
      return "同じ名前のファイルまたはフォルダが既にあります。";
    case 413:
      return "ファイルが大きすぎます。";
    case 429:
      return "操作が集中しています。しばらく待ってから再試行してください。";
    default:
      return status >= 500
        ? `サーバでエラーが発生しました。しばらく待ってから再試行してください (${status})。`
        : `操作に失敗しました (${status})。`;
  }
}

/// 名前の衝突（同じフォルダに同名がある・409）か。
export function isConflict(e: unknown): boolean {
  return e instanceof StorageApiError && e.status === 409;
}

function qs(params: Record<string, string | number | undefined>): string {
  const sp = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v !== undefined && v !== "") sp.set(k, String(v));
  }
  const s = sp.toString();
  return s ? `?${s}` : "";
}

// --- ブラウズ -------------------------------------------------------------

/// フォルダ（未指定はルート）の子を 1 ページ取得する（サーバ側ソート＋keyset）。
export function listChildren(opts: {
  parentId?: string;
  sort?: SortField;
  desc?: boolean;
  cursor?: string;
  limit?: number;
  /// 名前の部分一致検索。指定時はフォルダ階層を跨いで横断検索する。
  q?: string;
}): Promise<ChildrenResponse> {
  return apiFetch(
    `/nodes${qs({
      parent_id: opts.parentId,
      sort: opts.sort,
      desc: opts.desc ? "true" : undefined,
      cursor: opts.cursor,
      limit: opts.limit,
      q: opts.q,
    })}`,
  ).then((r) => okJson<ChildrenResponse>(r));
}

/// ノードのパンくず（root→自身。読める接頭のみ）。
export function breadcrumb(nodeId: string): Promise<CrumbResponse[]> {
  return apiFetch(`/nodes/${nodeId}/breadcrumb`).then((r) => okJson<CrumbResponse[]>(r));
}

/// 同じフォルダにある同名ノードを探す（アップロード前の衝突確認）。名前検索の先頭 1 ページから
/// 親と名前が完全一致するものだけを拾う（フォルダの子を全件読まない）。名前の一意制約は
/// 大文字小文字を区別するため比較も完全一致。見つからなければ null（読めない同名は見えないので、
/// その場合は finalize の 409 で気付く）。
export async function findSiblingByName(
  parentId: string | null,
  name: string,
): Promise<NodeResponse | null> {
  const page = await listChildren({ q: name, limit: 100 });
  return (
    page.items.find((n) => (n.parent_id ?? null) === parentId && n.name === name) ?? null
  );
}

// --- フォルダ操作 ---------------------------------------------------------

export function createFolder(name: string, parentId?: string): Promise<NodeResponse> {
  return apiFetch("/folders", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name, parent_id: parentId ?? null }),
  }).then((r) => okJson<NodeResponse>(r));
}

/// フォルダのリネーム/移動。`move` 省略=移動しない、`null`=ルートへ、文字列=そのフォルダへ。
export function updateFolder(
  id: string,
  patch: { name?: string; move?: string | null },
): Promise<NodeResponse> {
  return apiFetch(`/folders/${id}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(buildUpdateBody(patch)),
  }).then((r) => okJson<NodeResponse>(r));
}

export function deleteFolder(id: string): Promise<void> {
  return apiFetch(`/folders/${id}`, { method: "DELETE" }).then(okEmpty);
}

export function restoreFolder(id: string): Promise<NodeResponse> {
  return apiFetch(`/folders/${id}/restore`, { method: "POST" }).then((r) =>
    okJson<NodeResponse>(r),
  );
}

// --- ファイル操作 ---------------------------------------------------------

export function updateFile(
  id: string,
  patch: { name?: string; move?: string | null },
): Promise<NodeResponse> {
  return apiFetch(`/files/${id}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(buildUpdateBody(patch)),
  }).then((r) => okJson<NodeResponse>(r));
}

export function deleteFile(id: string): Promise<void> {
  return apiFetch(`/files/${id}`, { method: "DELETE" }).then(okEmpty);
}

export function restoreFile(id: string): Promise<NodeResponse> {
  return apiFetch(`/files/${id}/restore`, { method: "POST" }).then((r) =>
    okJson<NodeResponse>(r),
  );
}

export function downloadUrl(id: string): Promise<DownloadUrlResponse> {
  return apiFetch(`/files/${id}/download-url`).then((r) => okJson<DownloadUrlResponse>(r));
}

/// 1 ファイルのメタ情報（名前・content_type・サイズ等）を取得する。ビューアで使う。
export function getNode(id: string): Promise<NodeResponse> {
  return apiFetch(`/files/${id}`).then((r) => okJson<NodeResponse>(r));
}

// --- アップロード（2 段: declare → presigned PUT → finalize） --------------

/// 1 ファイルをアップロードする。`targetNodeId` 指定で既存ファイルへの新バージョン。
/// `name` 指定でファイル名を差し替える（同名衝突時に「両方残す」で連番名にする）。
export async function uploadFile(opts: {
  file: File;
  parentId?: string;
  targetNodeId?: string;
  name?: string;
  onProgress?: (fraction: number) => void;
}): Promise<NodeResponse> {
  const { file } = opts;
  const name = opts.name ?? file.name;
  const buffer = await file.arrayBuffer();
  const sha256 = await sha256Hex(buffer);
  const contentType = file.type || "application/octet-stream";

  // 1. declare（presigned PUT URL を得る）。
  const ticket = await apiFetch("/files", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      parent_id: opts.parentId ?? null,
      name: opts.targetNodeId ? null : name,
      content_type: contentType,
      size: file.size,
      sha256,
      target_node_id: opts.targetNodeId ?? null,
    }),
  }).then((r) => okJson<UploadTicketResponse>(r));

  // 2. presigned PUT で実体をオブジェクトストアへ直接転送（BFF を経由しない）。
  await putWithProgress(ticket.upload_url, buffer, contentType, opts.onProgress);

  // 3. finalize（server-side で再ハッシュ照合し node を確定）。
  return apiFetch(`/files/${ticket.upload_id}/finalize`, { method: "POST" }).then((r) =>
    okJson<NodeResponse>(r),
  );
}

/// presigned PUT を進捗付きで送る（XHR で upload.onprogress を取る）。
function putWithProgress(
  url: string,
  body: ArrayBuffer,
  contentType: string,
  onProgress?: (fraction: number) => void,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("PUT", url);
    xhr.setRequestHeader("Content-Type", contentType);
    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable && onProgress) onProgress(e.loaded / e.total);
    };
    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) resolve();
      else reject(new StorageApiError(xhr.status, `アップロードに失敗しました (${xhr.status})`));
    };
    xhr.onerror = () => reject(new StorageApiError(0, "アップロードの通信に失敗しました"));
    // ネットワーク停止や極端な低速で onload/onerror のどちらも発火せずハングするのを防ぐ。
    xhr.timeout = 120_000;
    xhr.ontimeout = () => reject(new StorageApiError(0, "アップロードがタイムアウトしました"));
    xhr.send(body);
  });
}

// --- バージョニング -------------------------------------------------------

export function listVersions(
  fileId: string,
  opts?: { cursor?: string; limit?: number },
): Promise<FileVersionsResponse> {
  return apiFetch(`/files/${fileId}/versions${qs({ cursor: opts?.cursor, limit: opts?.limit })}`).then(
    (r) => okJson<FileVersionsResponse>(r),
  );
}

export function versionDownloadUrl(fileId: string, version: number): Promise<DownloadUrlResponse> {
  return apiFetch(`/files/${fileId}/versions/${version}/download-url`).then((r) =>
    okJson<DownloadUrlResponse>(r),
  );
}

/// 版のブロック列を ordinal 順の窓で取る（出典パネル・#508）。
export function getVersionBlocks(
  fileId: string,
  version: number,
  opts: { from: number; limit: number },
): Promise<DocBlocksPage> {
  return apiFetch(`/files/${fileId}/versions/${version}/blocks${qs({ from: opts.from, limit: opts.limit })}`).then(
    (r) => okJson<DocBlocksPage>(r),
  );
}

export function restoreVersion(fileId: string, version: number): Promise<NodeResponse> {
  return apiFetch(`/files/${fileId}/versions/${version}/restore`, { method: "POST" }).then((r) =>
    okJson<NodeResponse>(r),
  );
}

/// AI 提案バージョンを採用して通常の新バージョンへ昇格する（Task 11.8・editor のみ）。
export function adoptVersion(fileId: string, version: number): Promise<NodeResponse> {
  return apiFetch(`/files/${fileId}/versions/${version}/adopt`, { method: "POST" }).then((r) =>
    okJson<NodeResponse>(r),
  );
}

// --- 共有 -----------------------------------------------------------------

export function listShares(nodeId: string): Promise<ShareEntry[]> {
  return apiFetch(`/nodes/${nodeId}/shares`).then((r) => okJson<ShareEntry[]>(r));
}

/// 共有相手（個人 user / ロール・部署 role）へ権限を付与する。
export function shareNode(nodeId: string, target: ShareTarget, role: ShareRole): Promise<void> {
  return apiFetch(`/nodes/${nodeId}/shares`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ target, role }),
  }).then(okEmpty);
}

export function unshareNode(nodeId: string, target: ShareTarget, role: ShareRole): Promise<void> {
  return apiFetch(`/nodes/${nodeId}/shares`, {
    method: "DELETE",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ target, role }),
  }).then(okEmpty);
}

// --- 共有リンク（複数発行・個別失効/延長・#342） --------------------------

/// 共有リンクを発行する（owner のみ）。発行結果（token 含む）を返す。
export function createShareLink(nodeId: string, body: CreateShareLinkBody): Promise<ShareLink> {
  return apiFetch(`/nodes/${nodeId}/share-links`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  }).then((r) => okJson<ShareLink>(r));
}

/// ノードの active な共有リンク一覧を取得する（owner のみ）。
export function listShareLinks(nodeId: string): Promise<ShareLink[]> {
  return apiFetch(`/nodes/${nodeId}/share-links`).then((r) => okJson<ShareLink[]>(r));
}

/// 共有リンクを失効する（owner のみ）。
export function revokeShareLink(linkId: string): Promise<void> {
  return apiFetch(`/share-links/${linkId}`, { method: "DELETE" }).then(okEmpty);
}

/// 共有リンクの有効期限を延長/変更する（owner のみ・null で無期限化）。
export function extendShareLink(linkId: string, expiresAt: string | null): Promise<void> {
  return apiFetch(`/share-links/${linkId}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ expires_at: expiresAt }),
  }).then(okEmpty);
}

/// パスワード付き共有リンクを token で解錠する（認証済みなら誰でも・失敗は一律 403）。
export function redeemShareLink(token: string, password?: string): Promise<void> {
  return apiFetch(`/share-links/redeem`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ token, password }),
  }).then(okEmpty);
}

/// リンクを redeem（解錠）した user 一覧を取得する（owner のみ・#369 C-3）。
export function listShareLinkGrants(linkId: string): Promise<ShareLinkGrant[]> {
  return apiFetch(`/share-links/${linkId}/grants`).then((r) => okJson<ShareLinkGrant[]>(r));
}

/// 特定 user の redeem を個別に取り消す（owner のみ・#375）。取消は durable で、対象ユーザーは
/// 同じ URL＋パスワードでもこのリンクからは再解錠できない（deny 台帳）。
export function revokeShareLinkGrant(linkId: string, userId: string): Promise<void> {
  return apiFetch(`/share-links/${linkId}/grants/${encodeURIComponent(userId)}`, {
    method: "DELETE",
  }).then(okEmpty);
}

/// 自分に共有されたノード一覧（keyset ページング）。
export function sharedWithMe(opts?: { cursor?: string; limit?: number }): Promise<ChildrenResponse> {
  return apiFetch(`/shares/shared-with-me${qs({ cursor: opts?.cursor, limit: opts?.limit })}`).then(
    (r) => okJson<ChildrenResponse>(r),
  );
}

// --- ディレクトリ（共有相手検索。自テナントのみ） -------------------------

export function searchDirectory(opts: {
  q: string;
  cursor?: string;
  limit?: number;
}): Promise<DirectorySearchResponse> {
  return apiFetch(`/directory/users${qs({ q: opts.q, cursor: opts.cursor, limit: opts.limit })}`).then(
    (r) => okJson<DirectorySearchResponse>(r),
  );
}

/// ロール/部署の相手検索（共有ダイアログのオートコンプリート・#76）。同テナントに絞られる。
export function searchRoles(opts: {
  q: string;
  cursor?: string;
  limit?: number;
}): Promise<DirectoryRoleSearchResponse> {
  return apiFetch(`/directory/roles${qs({ q: opts.q, cursor: opts.cursor, limit: opts.limit })}`).then(
    (r) => okJson<DirectoryRoleSearchResponse>(r),
  );
}

// --- ゴミ箱 ---------------------------------------------------------------

export function listTrash(opts?: { cursor?: string; limit?: number }): Promise<ChildrenResponse> {
  return apiFetch(`/trash${qs({ cursor: opts?.cursor, limit: opts?.limit })}`).then((r) =>
    okJson<ChildrenResponse>(r),
  );
}

/// kind に応じた復元（file/folder で別エンドポイント）。
export function restoreNode(node: NodeResponse): Promise<NodeResponse> {
  return node.kind === "folder" ? restoreFolder(node.id) : restoreFile(node.id);
}

// --- 補助 -----------------------------------------------------------------

/// `name`/`move` を PATCH ボディへ変換する。`move` 未指定なら parent_id を載せない（移動しない）。
function buildUpdateBody(patch: { name?: string; move?: string | null }): Record<string, unknown> {
  const body: Record<string, unknown> = {};
  if (patch.name !== undefined) body.name = patch.name;
  if (patch.move !== undefined) body.parent_id = patch.move; // null=ルート / 文字列=移動先
  return body;
}

/// presigned ダウンロード URL でブラウザのダウンロードを起動する。
///
/// `window.open` は使わない: URL の取得（await）を挟むとユーザー操作の文脈が切れ、
/// iOS Safari ではポップアップとしてブロックされて何も起きない。presigned GET は
/// `Content-Disposition: attachment` 付きで署名されるため（StorageService の presign_get）、
/// 同じタブでの遷移でも画面は離れずにダウンロードだけが始まる。
export function startDownload(url: string): void {
  const a = document.createElement("a");
  a.href = url;
  a.rel = "noopener noreferrer";
  // 別オリジン（オブジェクトストア）では download 属性は無視されるが、同一オリジン配信の
  // 構成でも保存として扱わせるために付けておく（ファイル名はヘッダ側が正）。
  a.download = "";
  a.style.display = "none";
  document.body.appendChild(a);
  a.click();
  a.remove();
}

/// presigned ダウンロード URL を取り、ブラウザのダウンロードを起動する。
export async function triggerDownload(fileId: string): Promise<void> {
  const { url } = await downloadUrl(fileId);
  startDownload(url);
}
