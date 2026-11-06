"use client";

/// Web ページの訳文のカード（web_translate・translation ブロック・docs/agent/web-translate.md §4.6）。
///
/// 訳文は**外部ページ由来**なので、Markdown は `page` モードで描く（フェンスは素のコード、リンクは
/// http(s) だけで行き先のホストを添える、引用番号・図・ボタンにしない）。画像は、ツールが**原文から**
/// 取り出した `source_images` に現れたものだけを shiki-server 経由で読み込む（訳文の本文は出所に数えない）。

import * as React from "react";
import { Check, ChevronDown, ChevronUp, Copy, ExternalLink, Languages, TriangleAlert } from "lucide-react";

import { Markdown } from "@/components/prompt-kit/markdown";
import { toast } from "@/components/ui/use-toast";
import { collectImageSources, type ImageProxy } from "@/lib/answer-images";
import { cn } from "@/lib/utils";

export type Translation = {
  sourceUrl: string;
  host: string;
  title: string | null;
  translatedTitle: string | null;
  targetLang: string;
  markdown: string;
  failedChunks: number;
  chunks: number;
  sourceImages: string[];
  sourceTruncated: boolean;
  fetchedAt: string | null;
};

const LANG_LABELS: Record<string, string> = {
  ja: "日本語",
  en: "英語",
  "zh-cn": "中国語（簡体字）",
  zh: "中国語（簡体字）",
  "zh-tw": "中国語（繁体字）",
  ko: "韓国語",
  fr: "フランス語",
  de: "ドイツ語",
  es: "スペイン語",
};

/// これより長い訳文は、最初は途中までを見せて「続きを表示」で開く（会話の流れを押し流さない）。
const COLLAPSE_CHARS = 2400;

function str(v: unknown): string | null {
  return typeof v === "string" && v.trim() ? v : null;
}

/// ブロックの JSON を読む（形が違えば null）。URL は http(s) だけを受け付ける。
export function parseTranslation(raw: unknown): Translation | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const sourceUrl = str(r.source_url);
  const markdown = typeof r.markdown === "string" ? r.markdown : null;
  if (!sourceUrl || markdown === null) return null;
  let host: string;
  try {
    const u = new URL(sourceUrl);
    if (u.protocol !== "http:" && u.protocol !== "https:") return null;
    host = u.host;
  } catch {
    return null;
  }
  const images = Array.isArray(r.source_images)
    ? r.source_images.filter((u): u is string => typeof u === "string")
    : [];
  return {
    sourceUrl,
    host,
    title: str(r.title),
    translatedTitle: str(r.translated_title),
    targetLang: str(r.target_lang) ?? "ja",
    markdown,
    failedChunks: Array.isArray(r.failed_chunks) ? r.failed_chunks.length : 0,
    chunks: typeof r.chunks === "number" ? r.chunks : 0,
    sourceImages: images,
    sourceTruncated: r.source_truncated === true,
    fetchedAt: str(r.fetched_at),
  };
}

function langLabel(code: string): string {
  return LANG_LABELS[code.toLowerCase()] ?? code;
}

/// コピーする本文（見出し・出典・機械翻訳の注記を添える）。
function copyText(t: Translation): string {
  const title = t.translatedTitle ?? t.title;
  const head = title ? `# ${title}\n\n` : "";
  return `${head}出典: ${t.sourceUrl}（機械翻訳）\n\n${t.markdown}\n`;
}

export function TranslationCard({ raw, imageProxy }: { raw: unknown; imageProxy: ImageProxy | null }) {
  const t = React.useMemo(() => parseTranslation(raw), [raw]);
  // 出所は原文の画像だけ（`![](url)` の形にして回答の画像と同じ規則で集める・サーバと同じ）。
  const imageSources = React.useMemo(
    () => collectImageSources((t?.sourceImages ?? []).map((u) => `![](${u})`)),
    [t],
  );
  const long = (t?.markdown.length ?? 0) > COLLAPSE_CHARS;
  const [open, setOpen] = React.useState(false);
  const [copied, setCopied] = React.useState(false);
  const bodyId = React.useId();
  if (!t) return null;
  const title = t.translatedTitle ?? t.title ?? t.host;
  const expanded = open || !long;

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(copyText(t));
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      toast({ description: "コピーに失敗しました" });
    }
  };

  return (
    <section
      className="my-3 overflow-hidden rounded-xl border bg-card"
      data-testid="translation-card"
      aria-label={`${title} の翻訳`}
    >
      <header className="flex items-start gap-3 border-b px-4 py-3">
        <span className="mt-0.5 flex size-9 shrink-0 items-center justify-center rounded-lg bg-sky-500/10 text-sky-600 dark:text-sky-400">
          <Languages className="size-4.5" aria-hidden />
        </span>
        <div className="min-w-0 flex-1">
          <h3 className="text-[15px] font-semibold leading-snug" data-testid="translation-title">
            {title}
          </h3>
          {t.translatedTitle && t.title && t.title !== t.translatedTitle ? (
            <p className="mt-0.5 truncate text-xs text-muted-foreground" title={t.title}>
              原題: {t.title}
            </p>
          ) : null}
          <p className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground">
            <a
              href={t.sourceUrl}
              target="_blank"
              rel="noopener noreferrer"
              className="inline-flex items-center gap-1 font-medium text-primary underline decoration-primary/30 underline-offset-2 hover:decoration-primary"
              data-testid="translation-source"
            >
              {t.host}
              <ExternalLink className="size-3" aria-hidden />
            </a>
            <span aria-hidden>·</span>
            <span>機械翻訳（{langLabel(t.targetLang)}）</span>
          </p>
        </div>
        <button
          type="button"
          onClick={copy}
          className="inline-flex shrink-0 items-center gap-1.5 rounded-full border px-3 py-1.5 text-xs font-medium transition-colors duration-fast hover:border-primary/40 hover:bg-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          aria-label="訳文をコピー"
        >
          {copied ? <Check className="size-3.5" aria-hidden /> : <Copy className="size-3.5" aria-hidden />}
          {copied ? "コピー済み" : "コピー"}
        </button>
      </header>
      {t.failedChunks > 0 || t.sourceTruncated ? (
        <div
          className="flex items-start gap-2 border-b bg-amber-500/5 px-4 py-2 text-xs text-amber-700 dark:text-amber-300"
          data-testid="translation-notice"
        >
          <TriangleAlert className="mt-px size-3.5 shrink-0" aria-hidden />
          <span>
            {t.failedChunks > 0 ? `${t.failedChunks} か所は翻訳できなかったため、原文のまま表示しています。` : null}
            {t.sourceTruncated ? "ページが長いため、取得できた範囲までを訳しています。" : null}
          </span>
        </div>
      ) : null}
      <div className="relative">
        <div
          id={bodyId}
          className={cn("px-4 py-3", !expanded && "max-h-[28rem] overflow-hidden")}
          data-testid="translation-body"
        >
          <Markdown page imageSources={imageSources} imageProxy={imageProxy}>
            {t.markdown}
          </Markdown>
        </div>
        {!expanded ? (
          <div className="pointer-events-none absolute inset-x-0 bottom-0 h-24 bg-gradient-to-t from-card to-transparent" />
        ) : null}
      </div>
      {long ? (
        <div className="flex justify-center border-t px-4 py-2">
          <button
            type="button"
            onClick={() => setOpen((v) => !v)}
            aria-expanded={expanded}
            aria-controls={bodyId}
            className="inline-flex items-center gap-1 rounded-full px-3 py-1 text-xs font-medium text-muted-foreground transition-colors duration-fast hover:bg-secondary hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            data-testid="translation-toggle"
          >
            {expanded ? (
              <>
                折りたたむ <ChevronUp className="size-3.5" aria-hidden />
              </>
            ) : (
              <>
                続きを表示 <ChevronDown className="size-3.5" aria-hidden />
              </>
            )}
          </button>
        </div>
      ) : null}
      <footer className="border-t bg-muted/30 px-4 py-2 text-[11px] text-muted-foreground">
        機械翻訳です。正確な内容は原文で確認してください。
      </footer>
    </section>
  );
}
