"use client";

/// テキスト・コードのプレビュー（閲覧のみ・#784）。
///
/// 中身を取り、行番号付きで出す。コードは shiki で色を付ける（ライト/ダークの 2 テーマを一度に出し、
/// `.dark` で切り替える）。大きいファイルは色を付けずに出し、上限を超えるものは開かない。
/// 中身は必ず文字列として描く（HTML や SVG のソースを実行しない）。
import * as React from "react";
import { Check, Copy, WrapText } from "lucide-react";

import { EditorLoading } from "@/components/shell/editor-loading";
import { toast } from "@/components/ui/use-toast";
import { formatBytes } from "@/lib/format";
import {
  HIGHLIGHT_MAX_BYTES,
  TEXT_MAX_BYTES,
  TEXT_MAX_LINES,
  languageColor,
  languageLabel,
  looksBinary,
  textLanguage,
} from "@/lib/file-preview";
import { downloadUrl, type NodeResponse } from "@/lib/storage";
import { cn } from "@/lib/utils";

import { useFileChrome } from "./file-chrome";

export type PreviewFailure = { title?: string; description?: string };

const tooLarge: PreviewFailure = {
  title: "大きすぎるためプレビューできません",
  description: `${TEXT_MAX_BYTES / 1024 / 1024}MB を超えるテキストは、ダウンロードして開いてください。`,
};

type Loaded = { text: string; encoding: "utf-8" | "shift_jis"; lines: number; html: string; highlighted: boolean };

/// 文字コードを決めて文字列にする。UTF-8 として壊れていれば Shift_JIS とみなす（日本の古い CSV・ログに多い）。
function decode(bytes: Uint8Array): { text: string; encoding: Loaded["encoding"] } {
  try {
    return { text: new TextDecoder("utf-8", { fatal: true }).decode(bytes), encoding: "utf-8" };
  } catch {
    return { text: new TextDecoder("shift_jis").decode(bytes), encoding: "shift_jis" };
  }
}

function escapeHtml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/// 色なしの行（shiki の出力と同じ `<span class="line">` の並びにして、見た目と行番号を揃える）。
function plainHtml(text: string): string {
  return `<pre class="shiki"><code>${text
    .split("\n")
    .map((l) => `<span class="line">${escapeHtml(l)}</span>`)
    .join("")}</code></pre>`;
}

async function highlight(text: string, lang: string): Promise<string> {
  const { codeToHtml } = await import("shiki");
  const html = await codeToHtml(text, {
    lang,
    themes: { light: "github-light", dark: "github-dark" },
    defaultColor: "light",
  });
  // 行は display:block で並べるので、行の間の改行文字は空行になる。取り除く。
  return html.replace(/<\/span>\n<span class="line"/g, '</span><span class="line"');
}

export function TextPreview({
  node,
  onDownload,
  onUnsupported,
}: {
  node: NodeResponse;
  onDownload: () => void;
  onUnsupported: (why: PreviewFailure) => void;
}) {
  const lang = textLanguage(node.name);
  const [loaded, setLoaded] = React.useState<Loaded | null>(null);
  // 散文（言語なし）とスマホでは折り返し、PC のコードは横にスクロールさせる。
  const [wrap, setWrap] = React.useState(
    () => lang === "text" || (typeof window !== "undefined" && window.matchMedia("(max-width: 767px)").matches),
  );
  const [copied, setCopied] = React.useState(false);
  const failRef = React.useRef(onUnsupported);
  failRef.current = onUnsupported;

  React.useEffect(() => {
    if ((node.size_bytes ?? 0) > TEXT_MAX_BYTES) {
      failRef.current(tooLarge);
      return;
    }
    let active = true;
    (async () => {
      const { url } = await downloadUrl(node.id);
      const res = await fetch(url);
      if (!res.ok) throw new Error(`取得に失敗しました（${res.status}）`);
      const bytes = new Uint8Array(await res.arrayBuffer());
      if (!active) return;
      // メタ情報のサイズが無い・古い場合に備えて、実際に取れた大きさでも確かめる。
      if (bytes.length > TEXT_MAX_BYTES) {
        failRef.current(tooLarge);
        return;
      }
      if (looksBinary(bytes)) {
        failRef.current({ description: "テキストではないため表示できません。ダウンロードして、お使いのアプリで開いてください。" });
        return;
      }
      const { text, encoding } = decode(bytes);
      const lines = text.length === 0 ? 1 : text.split("\n").length;
      if (lines > TEXT_MAX_LINES) {
        failRef.current({
          title: "行数が多すぎるためプレビューできません",
          description: `${TEXT_MAX_LINES.toLocaleString("ja-JP")} 行を超えるテキストは、ダウンロードして開いてください。`,
        });
        return;
      }
      const base = { text, encoding, lines } as const;
      if (!active) return;
      // まず色なしで出し、色付けが終わったら差し替える（大きめのファイルでも待たせない）。
      setLoaded({ ...base, html: plainHtml(text), highlighted: false });
      if (lang === "text" || bytes.length > HIGHLIGHT_MAX_BYTES) return;
      try {
        const html = await highlight(text, lang);
        if (active) setLoaded({ ...base, html, highlighted: true });
      } catch {
        // 未知の言語・shiki の失敗は色なしのまま。
      }
    })().catch(() => {
      if (active) failRef.current({ title: "ファイルを読み込めませんでした", description: "時間をおいてもう一度開くか、ダウンロードしてください。" });
    });
    return () => {
      active = false;
    };
  }, [node.id, node.size_bytes, lang]);

  const copyAll = React.useCallback(async () => {
    if (!loaded) return;
    try {
      await navigator.clipboard.writeText(loaded.text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      toast({ variant: "destructive", title: "コピーできませんでした" });
    }
  }, [loaded]);

  useFileChrome(node, onDownload, {}, []);

  if (!loaded) return <EditorLoading kind="doc" message="ファイルを開いています…" />;
  const gutter = String(loaded.lines).length;
  const meta = [
    `${loaded.lines.toLocaleString("ja-JP")} 行`,
    formatBytes(node.size_bytes),
    loaded.encoding === "shift_jis" ? "Shift_JIS" : null,
  ].filter(Boolean);
  return (
    <div className="h-full min-h-0 overflow-auto bg-muted/40 md:px-6 md:py-5" data-testid="text-preview" tabIndex={0}>
      <div className="mx-auto flex min-h-full w-full max-w-6xl flex-col bg-card md:min-h-0 md:overflow-hidden md:rounded-xl md:border md:border-border/70 md:shadow-sm">
        {/* ファイルの見出し（言語・行数・サイズと操作）。本文をスクロールしても上に残す。 */}
        <div className="sticky top-0 z-[1] flex items-center gap-2 border-b border-border/70 bg-card/95 py-1.5 pl-4 pr-1.5 backdrop-blur max-md:pl-3">
          <span className="inline-flex items-center gap-1.5 rounded-md bg-muted px-2 py-0.5 text-xs font-medium text-foreground/80">
            <span className="size-2 rounded-full" style={{ background: languageColor(lang) }} aria-hidden />
            {languageLabel(lang, node.name)}
          </span>
          <span className="min-w-0 truncate text-xs text-muted-foreground tabular-nums">{meta.join("・")}</span>
          <span className="ml-auto flex shrink-0 items-center gap-0.5">
            <HeaderButton label="折り返して表示" pressed={wrap} onClick={() => setWrap((w) => !w)} testId="text-wrap-toggle">
              <WrapText aria-hidden />
              <span className="max-sm:hidden">折り返し</span>
            </HeaderButton>
            <HeaderButton label={copied ? "コピーしました" : "すべてコピー"} onClick={() => void copyAll()} testId="text-copy">
              {copied ? <Check aria-hidden /> : <Copy aria-hidden />}
              <span className="max-sm:hidden">{copied ? "コピーしました" : "コピー"}</span>
            </HeaderButton>
          </span>
        </div>
        <div className="overflow-x-auto overscroll-x-contain">
          <div
            className={cn("file-code min-w-max py-2 font-mono text-[13px] leading-[1.65]", wrap && "file-code-wrap min-w-0")}
            style={{ ["--gutter" as string]: `${gutter}ch` }}
            data-highlighted={loaded.highlighted ? "" : undefined}
            // 中身は shiki か escapeHtml を通した文字列だけ（ファイルの HTML をそのまま差し込まない）。
            dangerouslySetInnerHTML={{ __html: loaded.html }}
          />
        </div>
      </div>
    </div>
  );
}

function HeaderButton({
  label,
  pressed,
  onClick,
  testId,
  children,
}: {
  label: string;
  pressed?: boolean;
  onClick: () => void;
  testId?: string;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      aria-pressed={pressed}
      title={label}
      onClick={onClick}
      data-testid={testId}
      className={cn(
        "inline-flex h-7 items-center gap-1.5 rounded-md px-2 text-xs font-medium outline-none transition-colors",
        "text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring",
        "pointer-coarse:h-10 pointer-coarse:px-3 [&_svg]:size-3.5",
        pressed && "bg-accent text-foreground",
      )}
    >
      {children}
    </button>
  );
}
