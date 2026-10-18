// prompt-kit (https://www.prompt-kit.com) 由来の CodeBlock を本リポジトリ向けに実装。MIT License。
// シンタックスハイライトは shiki を遅延読み込みし、コピー操作を備える。コードブロックは
// ライト/ダーク両モードで読みやすい固定ダーク背景にする（チャット UI の慣例）。
"use client";

import * as React from "react";
import { Check, Copy } from "lucide-react";

import { cn } from "@/lib/utils";

/// ハイライトを始めるまでの間（コードが続けて変わる間は待つ）。
const HIGHLIGHT_DEBOUNCE_MS = 150;

export function CodeBlock({
  code,
  lang,
  pending = false,
}: {
  code: string;
  lang?: string;
  /// 生成中でフェンスがまだ閉じていない（＝中身が伸び続ける）。閉じるまではプレーンで出し、
  /// トークンごとにハイライトし直さない。
  pending?: boolean;
}) {
  // どのコードに対するハイライトかも持つ（中身が変わったら古い HTML を出さない）。
  const [highlighted, setHighlighted] = React.useState<{ key: string; html: string } | null>(null);
  const [copied, setCopied] = React.useState(false);
  const key = `${lang ?? ""}\u0000${code}`;
  const html = highlighted?.key === key ? highlighted.html : null;

  React.useEffect(() => {
    if (pending) return;
    let active = true;
    const timer = window.setTimeout(() => {
      import("shiki")
        .then(async ({ codeToHtml }) => {
          try {
            const out = await codeToHtml(code, {
              lang: lang || "text",
              theme: "github-dark",
            });
            if (active) setHighlighted({ key, html: out });
          } catch {
            // 未知の言語などはプレーン表示にフォールバック。
            if (active) setHighlighted(null);
          }
        })
        .catch(() => {});
    }, HIGHLIGHT_DEBOUNCE_MS);
    return () => {
      active = false;
      window.clearTimeout(timer);
    };
  }, [code, lang, key, pending]);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      /* クリップボード不可は無視 */
    }
  };

  return (
    <div className="group/code my-3 max-w-full overflow-hidden rounded-xl border border-border bg-[#0d1117] text-[13px] [overflow-wrap:normal]">
      <div className="flex items-center justify-between border-b border-white/10 px-3 py-1.5">
        <span className="font-mono text-[11px] uppercase tracking-wide text-white/45">
          {lang || "code"}
        </span>
        <button
          type="button"
          onClick={copy}
          aria-label="コードをコピー"
          className="flex items-center gap-1 rounded-md px-1.5 py-1 text-[11px] text-white/55 transition-colors hover:bg-white/10 hover:text-white/85 pointer-coarse:min-h-9 pointer-coarse:px-2.5"
        >
          {copied ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
          {copied ? "コピー済み" : "コピー"}
        </button>
      </div>
      {html ? (
        <div
          className="overflow-x-auto overscroll-x-contain [&_pre]:!m-0 [&_pre]:!bg-transparent [&_pre]:px-4 [&_pre]:py-3"
          // shiki が生成する安全な HTML（コード文字列のみ）。
          dangerouslySetInnerHTML={{ __html: html }}
        />
      ) : (
        <pre className={cn("overflow-x-auto overscroll-x-contain px-4 py-3 text-white/90")}>
          <code className="font-mono">{code}</code>
        </pre>
      )}
    </div>
  );
}
