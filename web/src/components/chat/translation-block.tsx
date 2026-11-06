"use client";

/// Web ページの訳文（web_translate・translation ブロック・docs/agent/web-translate.md §4.6）。
///
/// 訳文は**回答の本文と同じ見た目**で流す（枠で囲まない・折りたたまない）。出典と機械翻訳の注記は
/// 先頭の 1 行だけにする。モデルは訳文を書き写さない（ツールがここへ直接出す）。
///
/// 訳文は**外部ページ由来**なので、Markdown は `page` モードで描く（フェンスは素のコード、リンクは
/// http(s) だけで行き先のホストを添える、引用番号・図・ボタンにしない）。画像は、ツールが**原文から**
/// 取り出した `source_images` に現れたものだけを shiki-server 経由で読み込む（訳文の本文は出所に数えない）。

import * as React from "react";
import { Check, Copy, ExternalLink, Languages, TriangleAlert } from "lucide-react";

import { Markdown } from "@/components/prompt-kit/markdown";
import { toast } from "@/components/ui/use-toast";
import { collectImageSources, type ImageProxy } from "@/lib/answer-images";

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
  /// 長いページの部（1 始まり）と総数。
  part: number;
  parts: number;
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
    part: typeof r.part === "number" ? r.part : 1,
    parts: typeof r.parts === "number" ? r.parts : 1,
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

export function TranslationBlock({ raw, imageProxy }: { raw: unknown; imageProxy: ImageProxy | null }) {
  const t = React.useMemo(() => parseTranslation(raw), [raw]);
  // 出所は原文の画像だけ（`![](url)` の形にして回答の画像と同じ規則で集める・サーバと同じ）。
  const imageSources = React.useMemo(
    () => collectImageSources((t?.sourceImages ?? []).map((u) => `![](${u})`)),
    [t],
  );
  const [copied, setCopied] = React.useState(false);
  if (!t) return null;
  const title = t.translatedTitle ?? t.title;
  const more = t.part < t.parts;

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
    <section className="my-2" data-testid="translation-block" aria-label={`${title ?? t.host} の翻訳`}>
      <p className="flex flex-wrap items-center gap-x-1.5 gap-y-1 text-[13px] text-muted-foreground">
        <Languages className="size-3.5 shrink-0 text-sky-600 dark:text-sky-400" aria-hidden />
        <a
          href={t.sourceUrl}
          target="_blank"
          rel="noopener noreferrer"
          className="inline-flex items-center gap-1 font-medium text-foreground/80 underline decoration-border underline-offset-2 hover:decoration-foreground/60"
          data-testid="translation-source"
        >
          {t.host}
          <ExternalLink className="size-3" aria-hidden />
        </a>
        <span>の機械翻訳（{langLabel(t.targetLang)}）</span>
        <button
          type="button"
          onClick={copy}
          className="ml-1 inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 text-xs transition-colors duration-fast hover:bg-secondary hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          aria-label="訳文をコピー"
        >
          {copied ? <Check className="size-3.5" aria-hidden /> : <Copy className="size-3.5" aria-hidden />}
          {copied ? "コピー済み" : "コピー"}
        </button>
      </p>
      {t.failedChunks > 0 || t.sourceTruncated ? (
        <p
          className="mt-1.5 flex items-start gap-1.5 text-[13px] text-amber-700 dark:text-amber-300"
          data-testid="translation-notice"
        >
          <TriangleAlert className="mt-0.5 size-3.5 shrink-0" aria-hidden />
          <span>
            {t.failedChunks > 0 ? "一部は翻訳できなかったため、原文のまま表示しています。" : null}
            {t.sourceTruncated && !more ? "ページが非常に長いため、取得できた範囲までを訳しています。" : null}
          </span>
        </p>
      ) : null}
      <div className="mt-3" data-testid="translation-body">
        {title && t.part === 1 ? (
          <h2 className="mb-3 text-[1.35rem] font-semibold leading-snug text-foreground" data-testid="translation-title">
            {title}
          </h2>
        ) : null}
        <Markdown page imageSources={imageSources} imageProxy={imageProxy}>
          {t.markdown}
        </Markdown>
      </div>
      {more ? (
        <p className="mt-3 text-[13px] text-muted-foreground" data-testid="translation-more">
          続きがあります（{t.part}/{t.parts}）。「続けて」と送ると続きを訳します。
        </p>
      ) : null}
    </section>
  );
}
