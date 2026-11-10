/// Web ページの訳文（web_translate の translation ブロック・docs/agent/web-translate.md §4.6）の読み取り。
/// 会話の表示（`TranslationBlock`）と書き出し（`answer-export`）で共用する。

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

/// 回答のブロックから訳文を読み出す（形の違うものは落とす）。
export function translationsOf(blocks: readonly { type: string }[]): Translation[] {
  return blocks.flatMap((b) => {
    if (b.type !== "translation") return [];
    const t = parseTranslation((b as { translation?: unknown }).translation);
    return t ? [t] : [];
  });
}

export function langLabel(code: string): string {
  return LANG_LABELS[code.toLowerCase()] ?? code;
}

/// 訳文の見出しに出す題名（訳した題名 → 原題）。
export function translationTitle(t: Pick<Translation, "translatedTitle" | "title">): string | null {
  return t.translatedTitle ?? t.title;
}
