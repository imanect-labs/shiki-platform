/// Office 編集セッション API クライアント（Task 11.6/11.7・design §4.8）。
///
/// `/office/sessions` は Collabora iframe の組み立て材料（編集アクション URL・
/// WOPI access_token・WOPISrc）を返す。トークンは UX 用の入場券であり権限の根拠では
/// ない（WOPI 側が毎呼び出しで ReBAC 再チェックする・共有解除は即時反映）。

import { apiFetch } from "@/lib/api";
import type { components } from "@/generated/api";

export type OfficeSession = components["schemas"]["OfficeSessionResponse"];

/// Office 編集セッションを発行する。
///
/// - 404: ファイル無し・権限無し（存在秘匿）・未対応拡張子 → 呼び出し側はダウンロードへ誘導。
/// - 503: Collabora が応答しない（office profile 未起動等） → その旨を表示。
export async function createOfficeSession(fileId: string): Promise<OfficeSession> {
  const res = await apiFetch("/office/sessions", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ file_id: fileId }),
  });
  if (res.status === 404) throw new OfficeSessionError("notfound");
  if (res.status === 503) throw new OfficeSessionError("unavailable");
  if (!res.ok) {
    throw new Error(`編集セッションの発行に失敗しました (${res.status})`);
  }
  return (await res.json()) as OfficeSession;
}

/// セッション発行の想定内エラー（UI が状態別の空表示に写像する）。
export class OfficeSessionError extends Error {
  constructor(public readonly kind: "notfound" | "unavailable") {
    super(kind);
    this.name = "OfficeSessionError";
  }
}

/// Collabora iframe へ form POST する URL（アクション URL＋WOPISrc）を組み立てる。
///
/// discovery の urlsrc は `...cool.html?` のように `?` 終端が慣例だが、URL API で
/// クエリとして正しく付与し形の揺れに依存しない。
export function buildOfficeFrameUrl(session: OfficeSession): string {
  const url = new URL(session.action_url);
  url.searchParams.set("WOPISrc", session.wopi_src);
  // Collabora の UI 言語をブラウザに合わせる（既定は英語になってしまう）。
  url.searchParams.set("lang", typeof navigator === "undefined" ? "ja" : navigator.language);
  return url.toString();
}
