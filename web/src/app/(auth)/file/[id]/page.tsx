"use client";

/// 専用ビューアを持たないファイルの画面（#775・#784）。
///
/// ドライブの「開く」や共有リンクで、ノート・CSV・スライド・Office・PDF 以外の形式を開いたときに来る。
/// ブラウザが表示できる形式（テキスト・画像・動画・音声）はその場で中身を見せ、それ以外は
/// Google ドライブの「プレビューできません」と同じく、開いただけではダウンロードを始めずに
/// ファイルの情報と「ダウンロード」ボタンを出す。表示できるはずの形式でも、ブラウザが扱えない・
/// 大きすぎる・中身がバイナリだった場合は「プレビューできません」に切り替える。
/// ビューアのある形式で直接来たら、そのビューアへ置き換える（Office は編集サービス未起動時の
/// フォールバック先として、この画面に留める）。
import * as React from "react";
import { useParams, useRouter } from "next/navigation";
import { FileWarning } from "lucide-react";

import { useFileChrome } from "@/components/file-preview/file-chrome";
import { ImagePreview } from "@/components/file-preview/image-preview";
import { MediaPreview } from "@/components/file-preview/media-preview";
import { NoPreview } from "@/components/file-preview/no-preview";
import { TextPreview, type PreviewFailure } from "@/components/file-preview/text-preview";
import { EditorLoading, EditorUnavailable } from "@/components/shell/editor-loading";
import { toast } from "@/components/ui/use-toast";
import { previewKind } from "@/lib/file-preview";
import { resourcePath } from "@/lib/resource-link";
import { getNode, triggerDownload, type NodeResponse } from "@/lib/storage";

type State = { phase: "loading" } | { phase: "ready"; node: NodeResponse } | { phase: "error" };

export default function FilePage() {
  const { id } = useParams<{ id: string }>();
  const router = useRouter();
  const [state, setState] = React.useState<State>({ phase: "loading" });
  const [downloading, setDownloading] = React.useState(false);
  // 表示できるはずの形式で、表示に失敗したときの理由（「プレビューできません」の文言）。
  const [failure, setFailure] = React.useState<PreviewFailure | null>(null);

  React.useEffect(() => {
    let active = true;
    setState({ phase: "loading" });
    setFailure(null);
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
  const onDownload = React.useCallback(() => void download(), [download]);

  const node = state.phase === "ready" ? state.node : null;
  const kind = node && !failure ? previewKind(node) : null;

  if (node && kind === "text") return <TextPreview key={node.id} node={node} onDownload={onDownload} onUnsupported={setFailure} />;
  if (node && kind === "image") return <ImagePreview key={node.id} node={node} onDownload={onDownload} onUnsupported={setFailure} />;
  if (node && (kind === "video" || kind === "audio"))
    return <MediaPreview key={node.id} kind={kind} node={node} onDownload={onDownload} onUnsupported={setFailure} />;
  return <Fallback state={state} failure={failure} downloading={downloading} onDownload={onDownload} />;
}

/// 読み込み中・開けない・プレビューできない。クロームはここで登録する（プレビュー側と二重にしない）。
function Fallback({
  state,
  failure,
  downloading,
  onDownload,
}: {
  state: State;
  failure: PreviewFailure | null;
  downloading: boolean;
  onDownload: () => void;
}) {
  const node = state.phase === "ready" ? state.node : null;
  useFileChrome(node, onDownload, {}, []);
  if (state.phase === "loading") return <EditorLoading kind="doc" message="ファイルを開いています…" />;
  if (!node) {
    return (
      <EditorUnavailable
        icon={FileWarning}
        title="このファイルは開けません"
        description="ファイルが存在しないか、開く権限がないか、読み込みに失敗しました。"
      />
    );
  }
  return <NoPreview node={node} onDownload={onDownload} downloading={downloading} {...failure} />;
}
