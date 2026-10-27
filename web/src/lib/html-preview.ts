/// ドライブの HTML ファイルを描画して見せるための文書づくり。
///
/// 描画は `<iframe sandbox srcdoc>`（opaque origin・`allow-scripts` なし）で行い、ここでは 2 層目として
/// 文書の先頭に CSP を差し込む。どちらか 1 層が壊れてもスクリプトが動かないようにする（PIT-40 と同じ考え方）。
/// 見た目に要る外部の画像・CSS・フォント（https）は読ませ、スクリプト・フォーム送信・埋め込みは止める。
/// リンクは iframe の中で遷移させず、新しいタブで開く（ページ内の `#` リンクだけは中で動かす）。

/// srcdoc の文書に差し込む CSP。iframe の sandbox と重ねて効かせる。
export const HTML_PREVIEW_CSP = [
  "default-src 'none'",
  "script-src 'none'",
  "img-src https: data: blob:",
  "media-src https: data: blob:",
  "style-src 'unsafe-inline' https:",
  "font-src https: data:",
  "object-src 'none'",
  "frame-src 'none'",
  "form-action 'none'",
].join("; ");

/// iframe に付ける sandbox。スクリプト・同一オリジン・フォーム・トップ遷移は許さない。
/// ポップアップはリンクを新しいタブで開くためだけに許す（開いた先は通常のページとして動かす）。
export const HTML_PREVIEW_SANDBOX = "allow-popups allow-popups-to-escape-sandbox";

/// 新しいタブで開いてよいリンクの scheme。
const LINK_SCHEMES = ["http:", "https:", "mailto:", "tel:"];

function doctypeOf(doc: Document): string {
  const dt = doc.doctype;
  if (!dt) return "";
  const pub = dt.publicId ? ` PUBLIC "${dt.publicId}"` : "";
  const sys = dt.systemId ? `${dt.publicId ? "" : " SYSTEM"} "${dt.systemId}"` : "";
  return `<!DOCTYPE ${dt.name}${pub}${sys}>`;
}

/// ファイルの HTML を、プレビュー用の srcdoc 文書にする。
/// DOMParser はスクリプトを実行しないので、ここでの解析自体は安全。
export function buildHtmlPreviewDoc(html: string): string {
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

  for (const a of doc.querySelectorAll<HTMLAnchorElement | HTMLAreaElement>("a[href], area[href]")) {
    const href = a.getAttribute("href")?.trim() ?? "";
    if (href.startsWith("#")) {
      a.removeAttribute("target");
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

  const meta = doc.createElement("meta");
  meta.setAttribute("http-equiv", "Content-Security-Policy");
  meta.setAttribute("content", HTML_PREVIEW_CSP);
  const base = doc.createElement("base");
  base.setAttribute("href", "about:srcdoc");
  doc.head.prepend(meta, base);

  return doctypeOf(doc) + doc.documentElement.outerHTML;
}
