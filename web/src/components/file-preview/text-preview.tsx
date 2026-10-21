"use client";

/// テキスト・コードのプレビュー（閲覧のみ・#784）。
///
/// 中身を取り、行番号付きで出す。コードは shiki で色を付ける（ライト/ダークの 2 テーマを一度に出し、
/// `.dark` で切り替える）。大きいファイルは色を付けずに出し、上限を超えるものは開かない。
/// 中身は必ず文字列として描く（HTML や SVG のソースを実行しない）。
import * as React from "react";
import { Check, Copy, WrapText } from "lucide-react";

import { nodeKindLabel } from "@/components/drive/primitives";
import { EditorLoading, EditorStatusBadge } from "@/components/shell/editor-loading";
import { ChromeIconButton } from "@/components/shell/page-header-context";
import { toast } from "@/components/ui/use-toast";
import { HIGHLIGHT_MAX_BYTES, TEXT_MAX_BYTES, looksBinary, textLanguage } from "@/lib/file-preview";
import { downloadUrl, type NodeResponse } from "@/lib/storage";
import { cn } from "@/lib/utils";

import { useFileChrome } from "./file-chrome";

export type PreviewFailure = { title?: string; description?: string };

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
  // 散文（言語なし）は折り返し、コードは横にスクロールさせる。
  const [wrap, setWrap] = React.useState(lang === "text");
  const [copied, setCopied] = React.useState(false);
  const failRef = React.useRef(onUnsupported);
  failRef.current = onUnsupported;

  React.useEffect(() => {
    if ((node.size_bytes ?? 0) > TEXT_MAX_BYTES) {
      failRef.current({
        title: "大きすぎるためプレビューできません",
        description: "5MB を超えるテキストは、ダウンロードして開いてください。",
      });
      return;
    }
    let active = true;
    (async () => {
      const { url } = await downloadUrl(node.id);
      const res = await fetch(url);
      if (!res.ok) throw new Error(`取得に失敗しました（${res.status}）`);
      const bytes = new Uint8Array(await res.arrayBuffer());
      if (looksBinary(bytes)) {
        failRef.current({ description: "テキストではないため表示できません。ダウンロードして、お使いのアプリで開いてください。" });
        return;
      }
      const { text, encoding } = decode(bytes);
      const lines = text.length === 0 ? 1 : text.split("\n").length;
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

  useFileChrome(
    node,
    onDownload,
    {
      subtitle: loaded ? (
        <span className="inline-flex items-center gap-2">
          <span>
            {nodeKindLabel(node)}・{loaded.lines.toLocaleString("ja-JP")} 行
          </span>
          {loaded.encoding === "shift_jis" ? <EditorStatusBadge>Shift_JIS</EditorStatusBadge> : null}
        </span>
      ) : undefined,
      actions: loaded ? (
        <>
          <ChromeIconButton label="折り返して表示" pressed={wrap} onClick={() => setWrap((w) => !w)} data-testid="text-wrap-toggle">
            <WrapText aria-hidden />
          </ChromeIconButton>
          <ChromeIconButton label={copied ? "コピーしました" : "すべてコピー"} onClick={() => void copyAll()} data-testid="text-copy">
            {copied ? <Check aria-hidden /> : <Copy aria-hidden />}
          </ChromeIconButton>
        </>
      ) : undefined,
    },
    [loaded, wrap, copied, copyAll],
  );

  if (!loaded) return <EditorLoading kind="doc" message="ファイルを開いています…" />;
  const gutter = String(loaded.lines).length;
  return (
    <div className="h-full min-h-0 overflow-auto bg-background" data-testid="text-preview" tabIndex={0}>
      <div
        className={cn("file-code min-w-max py-3 pr-6 font-mono text-[13px] leading-[1.6]", wrap && "file-code-wrap min-w-0")}
        style={{ ["--gutter" as string]: `${gutter}ch` }}
        data-highlighted={loaded.highlighted ? "" : undefined}
        // 中身は shiki か escapeHtml を通した文字列だけ（ファイルの HTML をそのまま差し込まない）。
        dangerouslySetInnerHTML={{ __html: loaded.html }}
      />
    </div>
  );
}
