// prompt-kit (https://www.prompt-kit.com) 由来の CodeBlock を本リポジトリ向けに実装。MIT License。
// シンタックスハイライトは shiki を遅延読み込みし、コピー操作を備える。コードブロックは
// ライト/ダーク両モードで読みやすい固定ダーク背景にする（チャット UI の慣例）。
"use client";

import * as React from "react";
import { Check, Copy } from "lucide-react";

import { cn } from "@/lib/utils";

export function CodeBlock({ code, lang }: { code: string; lang?: string }) {
  const [html, setHtml] = React.useState<string | null>(null);
  const [copied, setCopied] = React.useState(false);

  React.useEffect(() => {
    let active = true;
    import("shiki")
      .then(async ({ codeToHtml }) => {
        try {
          const out = await codeToHtml(code, {
            lang: lang || "text",
            theme: "github-dark",
          });
          if (active) setHtml(out);
        } catch {
          // 未知の言語などはプレーン表示にフォールバック。
          if (active) setHtml(null);
        }
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, [code, lang]);

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
    <div className="group/code my-3 overflow-hidden rounded-xl border border-border bg-[#0d1117] text-[13px]">
      <div className="flex items-center justify-between border-b border-white/10 px-3 py-1.5">
        <span className="font-mono text-[11px] uppercase tracking-wide text-white/45">
          {lang || "code"}
        </span>
        <button
          type="button"
          onClick={copy}
          aria-label="コードをコピー"
          className="flex items-center gap-1 rounded-md px-1.5 py-1 text-[11px] text-white/55 transition-colors hover:bg-white/10 hover:text-white/85"
        >
          {copied ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
          {copied ? "コピー済み" : "コピー"}
        </button>
      </div>
      {html ? (
        <div
          className="overflow-x-auto [&_pre]:!m-0 [&_pre]:!bg-transparent [&_pre]:px-4 [&_pre]:py-3"
          // shiki が生成する安全な HTML（コード文字列のみ）。
          dangerouslySetInnerHTML={{ __html: html }}
        />
      ) : (
        <pre className={cn("overflow-x-auto px-4 py-3 text-white/90")}>
          <code className="font-mono">{code}</code>
        </pre>
      )}
    </div>
  );
}
