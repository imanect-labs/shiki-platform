"use client";

/// ビューアの無いファイルの画面（Google ドライブの「プレビューできません」相当）。
///
/// ドライブの「開く」や共有リンクで、専用ビューア（ノート・CSV・スライド・Office・PDF）を
/// 持たない形式を開いたときに来る。開いただけでダウンロードは始めず、ファイルの情報と
/// 「ダウンロード」ボタンを出す。ビューアのある形式で直接来たら、そのビューアへ置き換える
/// （Office は編集サービス未起動時のフォールバック先として、この画面に留める）。
import * as React from "react";
import Link from "next/link";
import { useParams, useRouter } from "next/navigation";
import {
  Download,
  EyeOff,
  FileWarning,
  FolderOpen,
  Loader2,
} from "lucide-react";

import { NodeIcon, nodeKindLabel } from "@/components/drive/primitives";
import {
  EditorLoading,
  EditorUnavailable,
  useEditorBack,
} from "@/components/shell/editor-loading";
import { usePageChrome } from "@/components/shell/page-header-context";
import { Button } from "@/components/ui/button";
import { toast } from "@/components/ui/use-toast";
import { formatBytes } from "@/lib/format";
import { resourcePath } from "@/lib/resource-link";
import { getNode, triggerDownload, type NodeResponse } from "@/lib/storage";

type State =
  | { phase: "loading" }
  | { phase: "ready"; node: NodeResponse }
  | { phase: "error" };

const dateFormatter = new Intl.DateTimeFormat("ja-JP", {
  year: "numeric",
  month: "numeric",
  day: "numeric",
  hour: "2-digit",
  minute: "2-digit",
});

export default function FilePage() {
  const { id } = useParams<{ id: string }>();
  const router = useRouter();
  const [state, setState] = React.useState<State>({ phase: "loading" });
  const [downloading, setDownloading] = React.useState(false);

  React.useEffect(() => {
    let active = true;
    setState({ phase: "loading" });
    getNode(id)
      .then((node) => {
        if (!active) return;
        // ビューアのある形式（古いリンク・URL 直打ち）はそのビューアで開き直す。
        // Office は除く: 編集サービス未起動時のフォールバック先がこの画面なので、戻すと
        // /office ↔ /file を往復してダウンロード導線に辿り着けない。
        const path = resourcePath(node);
        if (!path.startsWith("/file/") && !path.startsWith("/office/")) router.replace(path);
        else setState({ phase: "ready", node });
      })
      .catch(() => {
        if (active) setState({ phase: "error" });
      });
    return () => {
      active = false;
    };
  }, [id, router]);

  const download = React.useCallback(async () => {
    setDownloading(true);
    try {
      await triggerDownload(id);
    } catch (e) {
      toast({
        variant: "destructive",
        title: "ダウンロードに失敗しました",
        description: e instanceof Error ? e.message : undefined,
      });
    } finally {
      setDownloading(false);
    }
  }, [id]);

  // クローム（戻る → ⋯（ダウンロード））。読み込み中・エラーでも戻る。
  const back = useEditorBack();
  const node = state.phase === "ready" ? state.node : null;
  usePageChrome(() => {
    if (!node) return { back };
    return {
      title: node.name,
      menuTitle: node.name,
      back,
      subtitle: nodeKindLabel(node),
      menu: [
        {
          key: "download",
          label: "ダウンロード",
          icon: Download,
          onSelect: () => void download(),
          testId: "file-menu-download",
        },
      ],
    };
  }, [back, node, download]);

  if (state.phase === "loading")
    return <EditorLoading kind="doc" message="ファイルを開いています…" />;
  if (state.phase === "error" || !node) {
    return (
      <EditorUnavailable
        icon={FileWarning}
        title="このファイルは開けません"
        description="ファイルが存在しないか、開く権限がないか、読み込みに失敗しました。"
      />
    );
  }

  const folderHref = node.parent_id
    ? `/drive?folder=${node.parent_id}`
    : "/drive";
  const meta = [
    nodeKindLabel(node),
    formatBytes(node.size_bytes),
    `${dateFormatter.format(new Date(node.updated_at))} 更新`,
  ]
    .filter(Boolean)
    .join("・");

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex min-h-0 flex-1 items-center justify-center overflow-auto bg-muted/40 p-4 sm:p-6">
        <div
          className="flex w-full max-w-md flex-col items-center gap-5 rounded-2xl border border-border/60 bg-card px-6 py-10 text-center shadow-sm"
          data-testid="file-no-preview"
        >
          <div className="flex size-16 items-center justify-center rounded-2xl bg-muted">
            <NodeIcon
              kind="file"
              name={node.name}
              contentType={node.content_type}
              className="size-8"
            />
          </div>
          <div className="flex min-w-0 max-w-full flex-col gap-1">
            <h1 className="break-words text-base font-semibold leading-snug [overflow-wrap:anywhere]">
              {node.name}
            </h1>
            <p className="text-xs text-muted-foreground">{meta}</p>
          </div>
          <div className="flex flex-col items-center gap-1">
            <p className="inline-flex items-center gap-1.5 text-sm font-medium text-foreground">
              <EyeOff className="size-4 text-muted-foreground" aria-hidden />
              このファイルはプレビューできません
            </p>
            <p className="text-sm text-muted-foreground">
              ダウンロードして、お使いのアプリで開いてください。
            </p>
          </div>
          <div className="flex flex-wrap items-center justify-center gap-2">
            <Button
              onClick={() => void download()}
              disabled={downloading}
              data-testid="file-download"
            >
              {downloading ? (
                <Loader2 className="animate-spin" aria-hidden />
              ) : (
                <Download aria-hidden />
              )}
              ダウンロード
            </Button>
            <Button asChild variant="outline">
              <Link href={folderHref}>
                <FolderOpen aria-hidden />
                フォルダを開く
              </Link>
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}
