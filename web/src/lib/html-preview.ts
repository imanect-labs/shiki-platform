/// ドライブの HTML ファイルを描画して見せるための文書づくり。
///
/// 描画は `<iframe sandbox srcdoc>`（opaque origin・`allow-scripts` なし）で行い、ここでは 2 層目として
/// 文書の先頭に CSP を差し込む。どちらか 1 層が壊れてもスクリプトが動かないようにする（PIT-40 と同じ考え方）。
/// スクリプト・フォーム送信・埋め込みは常に止める。外部の画像・CSS・フォントは既定で止め、閲覧者が
/// 「表示する」を選んだときだけ https のものを読ませる（開いたことや IP を外へ知らせる既読確認・社内への
/// 踏み台に使わせない。メールの外部画像と同じ扱い）。
/// リンクは iframe の中で遷移させず、新しいタブで開く（ページ内の `#` リンクだけは中で動かす）。

/// srcdoc の文書に差し込む CSP。iframe の sandbox と重ねて効かせる。
export function htmlPreviewCsp(allowExternal: boolean): string {
  const ext = allowExternal ? " https:" : "";
  return [
    "default-src 'none'",
    "script-src 'none'",
    `img-src data: blob:${ext}`,
    `media-src data: blob:${ext}`,
    `style-src 'unsafe-inline'${ext}`,
    `font-src data:${ext}`,
    "object-src 'none'",
    "frame-src 'none'",
    "form-action 'none'",
  ].join("; ");
}

/// iframe に付ける sandbox。スクリプト・同一オリジン・フォーム・トップ遷移は許さない。
/// ポップアップはリンクを新しいタブで開くためだけに許す（開いた先は通常のページとして動かす）。
export const HTML_PREVIEW_SANDBOX = "allow-popups allow-popups-to-escape-sandbox";

/// 新しいタブで開いてよいリンクの scheme。
const LINK_SCHEMES = ["http:", "https:", "mailto:", "tel:"];
/// 外す先読み系の link の rel。
const PREFETCH_RELS = ["dns-prefetch", "preconnect", "prefetch", "prerender", "preload", "modulepreload"];
const HTML_NS = "http://www.w3.org/1999/xhtml";
const XLINK_NS = "http://www.w3.org/1999/xlink";

/// 外部の資源を指しうる属性（リンクの href は除く）。
const RESOURCE_ATTRS = ["src", "srcset", "poster", "background", "data", "href", "xlink:href"];
const EXTERNAL_URL = /https?:\/\//i;
/// CSS の中の外部参照（`url(https://…)`・`@import "https://…"`）。
const EXTERNAL_CSS = /(url\(\s*['"]?\s*|@import\s+['"]\s*)https?:\/\//i;

/// 外部の画像・CSS・フォントを参照しているか（「表示する」を出すかの目安。止めるのは CSP 側）。
function referencesExternal(doc: Document): boolean {
  for (const el of doc.querySelectorAll("*")) {
    const tag = el.localName;
    if (tag === "style" && EXTERNAL_CSS.test(el.textContent ?? "")) return true;
    if (EXTERNAL_CSS.test(el.getAttribute("style") ?? "")) return true;
    // リンク（a・area）の href は開く先であって、読み込む資源ではない。
    if (tag === "a" || tag === "area") continue;
    for (const name of RESOURCE_ATTRS) {
      if (EXTERNAL_URL.test(el.getAttribute(name) ?? "")) return true;
    }
  }
  return false;
}

export type HtmlPreviewDoc = {
  srcDoc: string;
  /// 外部の資源を参照している（止めているなら、閲覧者に「表示する」を出す）。
  hasExternal: boolean;
};

function doctypeOf(doc: Document): string {
  const dt = doc.doctype;
  if (!dt) return "";
  const pub = dt.publicId ? ` PUBLIC "${dt.publicId}"` : "";
  const sys = dt.systemId ? `${dt.publicId ? "" : " SYSTEM"} "${dt.systemId}"` : "";
  return `<!DOCTYPE ${dt.name}${pub}${sys}>`;
}

/// ファイルの HTML を、プレビュー用の srcdoc 文書にする。
/// DOMParser はスクリプトを実行しないので、ここでの解析自体は安全。
export function buildHtmlPreviewDoc(html: string, opts: { allowExternal: boolean }): HtmlPreviewDoc {
  const doc = new DOMParser().parseFromString(html, "text/html");

  // スクリプトは CSP と sandbox で動かないが、念のため取り除く。meta refresh は iframe の中を遷移させるので外す。
  doc.querySelectorAll("script").forEach((el) => el.remove());
  doc.querySelectorAll("meta[http-equiv]").forEach((el) => {
    if (el.getAttribute("http-equiv")?.trim().toLowerCase() === "refresh") el.remove();
  });
  // srcdoc の相対 URL は親（アプリのページ）の URL を基準に解決されるため、`#x` のリンクでも iframe が
  // アプリのページへ遷移してしまう。基準を about:srcdoc に固定し、`#x` は文書内の移動に、
  // それ以外の相対 URL（画像・CSS を含む）は解決できずにどこへも取りに行かないようにする。
  doc.querySelectorAll("base").forEach((el) => el.remove());

  // 先読み系の link は CSP の対象外のことがある（dns-prefetch など）。開いたことが外へ漏れないよう外す。
  doc.querySelectorAll("link[rel]").forEach((el) => {
    const rel = el.getAttribute("rel")?.toLowerCase().split(/\s+/) ?? [];
    if (rel.some((r) => PREFETCH_RELS.includes(r))) el.remove();
  });

  for (const a of doc.querySelectorAll("a, area")) {
    const href = (a.getAttribute("href") ?? a.getAttributeNS(XLINK_NS, "href"))?.trim();
    if (href == null) continue;
    if (href.startsWith("#")) {
      a.removeAttribute("target");
      continue;
    }
    // SVG のリンクは rel（noopener）を持てないので、ページ内以外は外す（`xlink:href` も見る）。
    if (a.namespaceURI !== HTML_NS) {
      a.removeAttribute("href");
      a.removeAttributeNS(XLINK_NS, "href");
      continue;
    }
    let scheme: string | null = null;
    try {
      scheme = new URL(href).protocol;
    } catch {
      // 相対 URL（同じフォルダの別ファイルなど）は解決先が無いので、リンクを外す。
    }
    if (scheme && LINK_SCHEMES.includes(scheme)) {
      a.setAttribute("target", "_blank");
      a.setAttribute("rel", "noopener noreferrer");
    } else {
      a.removeAttribute("href");
    }
  }

  const hasExternal = referencesExternal(doc);
  const meta = doc.createElement("meta");
  meta.setAttribute("http-equiv", "Content-Security-Policy");
  meta.setAttribute("content", htmlPreviewCsp(opts.allowExternal));
  const base = doc.createElement("base");
  base.setAttribute("href", "about:srcdoc");
  // dns-prefetch は CSP では止まらないので、文書側でも切る。
  const noPrefetch = doc.createElement("meta");
  noPrefetch.setAttribute("http-equiv", "x-dns-prefetch-control");
  noPrefetch.setAttribute("content", "off");
  doc.head.prepend(meta, noPrefetch, base);

  return { srcDoc: doctypeOf(doc) + doc.documentElement.outerHTML, hasExternal };
}
