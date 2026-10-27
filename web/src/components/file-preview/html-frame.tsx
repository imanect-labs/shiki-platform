"use client";

/// HTML ファイルの描画フレーム。
///
/// srcdoc ＋ sandbox（`allow-scripts`・`allow-same-origin` なし）の opaque origin で描画し、文書には
/// CSP も差し込む（`buildHtmlPreviewDoc`）。アプリのオリジンでファイルの HTML を動かさない。
/// 背景を指定しない HTML がダークテーマの上で読めなくならないよう、フレーム自体を白で塗る。
import * as React from "react";

import { buildHtmlPreviewDoc, HTML_PREVIEW_SANDBOX } from "@/lib/html-preview";

export function HtmlFrame({ html, title }: { html: string; title: string }) {
  const srcDoc = React.useMemo(() => buildHtmlPreviewDoc(html), [html]);
  return (
    <iframe
      title={title}
      sandbox={HTML_PREVIEW_SANDBOX}
      srcDoc={srcDoc}
      referrerPolicy="no-referrer"
      className="block min-h-0 w-full flex-1 border-0 bg-white"
      data-testid="html-preview-frame"
    />
  );
}
