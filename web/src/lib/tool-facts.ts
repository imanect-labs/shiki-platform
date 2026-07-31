/// ツール実行の「何が返ってきたか」を 1 行で見せるための抽出（issue #386）。
///
/// 実況の行は**呼び出し名だけ**では中身が分からない。件数と出典ホストが出て初めて
/// 「調べている」感が伝わる（参照 UI もこの 2 つを出している）。
///
/// ツールごとに parser を書き分けない。見るのは 2 つだけ:
///
///   1. 先頭行の `N 件`（`web 検索結果 8 件:` / `社内文書 3 件:` …）
///   2. 本文中に現れる URL
///
/// この 2 規則しか使わないので、結果テキストの書式が変わっても壊れず（何も出ないだけ）、
/// 後から増える検索系ツールにもそのまま効く。**バックエンドの変更を必要としない**のも要点で、
/// 結果テキストは既にこの情報を含んでいる。

/// 先頭行に現れる「N 件」。`web 検索結果 8 件:` の 8 を拾う。
const COUNT_RE = /^[^\n]*?(\d{1,4})\s*件/;
/// 本文中の URL（末尾の句読点・閉じ括弧は含めない）。
const URL_RE = /https?:\/\/[^\s)\]"'<>、。]+/g;
/// 出典チップの表示上限。多すぎると 1 行に収まらず、逆に何も読めなくなる。
const MAX_HOSTS = 3;

/// URL からドメインだけを取り出す（`www.` は落とす）。壊れた URL は null。
export function hostOf(raw: string): string | null {
  try {
    const host = new URL(raw).hostname;
    return host.replace(/^www\./, "") || null;
  } catch {
    return null;
  }
}

export type ToolFacts = {
  /// 結果件数（分かる時だけ）。
  count: number | null;
  /// 出典ホスト（重複除去・最大 [`MAX_HOSTS`] 件）。
  hosts: string[];
};

/// 呼び出し入力と結果テキストから件数・出典ホストを取り出す。
///
/// `input` を先に見るのは **`web_fetch` の取得先は呼んだ瞬間に分かる**ため。結果を待たずに
/// チップを出せるので、行の高さが後から変わらない（ガタつきの元を断つ）。
export function toolFacts(input: unknown, result?: string): ToolFacts {
  const hosts: string[] = [];
  const push = (url: string) => {
    const h = hostOf(url);
    if (h && !hosts.includes(h)) hosts.push(h);
  };

  const url = (input as { url?: unknown } | null | undefined)?.url;
  if (typeof url === "string") push(url);

  let count: number | null = null;
  if (result) {
    const m = COUNT_RE.exec(result);
    if (m) count = Number(m[1]);
    for (const hit of result.matchAll(URL_RE)) {
      if (hosts.length >= MAX_HOSTS) break;
      push(hit[0]);
    }
  }
  return { count, hosts: hosts.slice(0, MAX_HOSTS) };
}
