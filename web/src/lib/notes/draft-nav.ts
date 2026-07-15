/// 下書きノート画面（`/notes/draft`）への遷移 URL を組み立てる（issue #282）。
///
/// 下書きは (threadId, name) キー。画面はこの 2 つで対象の下書きを特定し、同じ会話の他の下書きは
/// タブで切り替える。URL は 1 箇所で組み立て、カード/ストリーム/リセット導線で共有する。
export const DRAFT_PATH = "/notes/draft";

export function draftHref(threadId: string, name: string): string {
  const params = new URLSearchParams({ thread: threadId, name });
  return `${DRAFT_PATH}?${params.toString()}`;
}
