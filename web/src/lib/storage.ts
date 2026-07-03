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
export type ShareEntry = components["schemas"]["ShareEntry"];
export type ShareRole = components["schemas"]["ShareRole"];
export type ShareTarget = components["schemas"]["ShareTarget"];
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
  return `リクエストが失敗しました (${res.status})`;
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
export async function uploadFile(opts: {
  file: File;
  parentId?: string;
  targetNodeId?: string;
  onProgress?: (fraction: number) => void;
}): Promise<NodeResponse> {
  const { file } = opts;
  const buffer = await file.arrayBuffer();
  const sha256 = await sha256Hex(buffer);
  const contentType = file.type || "application/octet-stream";

  // 1. declare（presigned PUT URL を得る）。
  const ticket = await apiFetch("/files", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      parent_id: opts.parentId ?? null,
      name: opts.targetNodeId ? null : file.name,
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

export function restoreVersion(fileId: string, version: number): Promise<NodeResponse> {
  return apiFetch(`/files/${fileId}/versions/${version}/restore`, { method: "POST" }).then((r) =>
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

/// presigned ダウンロード URL を取り、ブラウザのダウンロードを起動する。
export async function triggerDownload(fileId: string): Promise<void> {
  const { url } = await downloadUrl(fileId);
  window.open(url, "_blank", "noopener,noreferrer");
}
