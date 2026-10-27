"use client";

/// HTML ファイルの描画フレーム。
///
/// srcdoc ＋ sandbox（`allow-scripts`・`allow-same-origin` なし）の opaque origin で描画し、文書には
/// CSP も差し込む（`buildHtmlPreviewDoc`）。アプリのオリジンでファイルの HTML を動かさない。
/// 外部の画像・CSS・フォントは既定で止め、閲覧者が選んだときだけ読み込む。
/// アプリの画面の中に出るので、偽のログイン画面などに騙されないよう「ファイルの内容である」ことを
/// フレームの上に常に出す。背景を指定しない HTML がダークテーマの上で読めなくならないよう、
/// フレーム自体を白で塗る。
import * as React from "react";
import { ImageIcon, ShieldAlert } from "lucide-react";

import { buildHtmlPreviewDoc, HTML_PREVIEW_SANDBOX } from "@/lib/html-preview";

export function HtmlFrame({ html, title }: { html: string; title: string }) {
  const [allowExternal, setAllowExternal] = React.useState(false);
  const { srcDoc, hasExternal } = React.useMemo(
    () => buildHtmlPreviewDoc(html, { allowExternal }),
    [html, allowExternal],
  );
  return (
    <>
      <div
        // 「表示する」を押してボタンが消えても高さを変えない（中身がずれない）ように最小の高さを取る。
        className="flex min-h-10 shrink-0 items-center gap-2 border-b border-border/70 bg-muted/50 py-1.5 pl-4 pr-1.5 text-xs text-muted-foreground max-md:pl-3"
        data-testid="html-preview-notice"
      >
        <ShieldAlert className="size-3.5 shrink-0" aria-hidden />
        <p className="min-w-0 flex-1">
          <span className="md:hidden">ファイルの内容です。パスワードは入力しないでください。</span>
          <span className="max-md:hidden">
            ファイルの内容です（Shiki の画面ではありません）。パスワードなどの入力を求められても応じないでください。
          </span>
        </p>
        {hasExternal && !allowExternal ? (
          <button
            type="button"
            onClick={() => setAllowExternal(true)}
            title="このファイルが参照している外部の画像・CSS・フォントを読み込みます。読み込むと、開いたことが参照先に伝わります。"
            className="inline-flex h-7 shrink-0 items-center gap-1.5 rounded-md px-2 font-medium text-foreground/80 outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:h-10 pointer-coarse:px-3 [&_svg]:size-3.5"
            data-testid="html-allow-external"
          >
            <ImageIcon aria-hidden />
            <span className="md:hidden">外部画像</span>
            <span className="max-md:hidden">外部の画像を表示</span>
          </button>
        ) : null}
      </div>
      <iframe
        title={title}
        sandbox={HTML_PREVIEW_SANDBOX}
        srcDoc={srcDoc}
        referrerPolicy="no-referrer"
        className="block min-h-0 w-full flex-1 border-0 bg-white"
        data-testid="html-preview-frame"
      />
    </>
  );
}
