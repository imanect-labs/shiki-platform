"use client";

/// 動画・音声のプレビュー（動画はブラウザ標準のプレーヤー、音声は自前の操作・#784）。
///
/// 署名付き URL は短命なので、再生の途中（シークで読み直したとき）に期限が切れることがある。
/// 再生できた後の読み込みエラーは URL を取り直して同じ位置から続け、最初から再生できない
/// ものだけ「この形式は再生できません」にする。
import * as React from "react";

import { nodeKindLabel } from "@/components/drive/primitives";
import { EditorLoading } from "@/components/shell/editor-loading";
import { formatBytes } from "@/lib/format";
import { downloadUrl, type NodeResponse } from "@/lib/storage";

import { AudioPlayer } from "./audio-player";
import { useFileChrome } from "./file-chrome";
import type { PreviewFailure } from "./text-preview";

export function MediaPreview({
  kind,
  node,
  onDownload,
  onUnsupported,
}: {
  kind: "video" | "audio";
  node: NodeResponse;
  onDownload: () => void;
  onUnsupported: (why: PreviewFailure) => void;
}) {
  const [src, setSrc] = React.useState<string | null>(null);
  const mediaRef = React.useRef<HTMLMediaElement | null>(null);
  // 一度でも再生の準備ができたか（できていれば、エラーは URL の期限切れとみなして取り直す）。
  const playable = React.useRef(false);
  const resume = React.useRef<{ time: number; playing: boolean } | null>(null);
  const failRef = React.useRef(onUnsupported);
  failRef.current = onUnsupported;

  const load = React.useCallback(() => {
    return downloadUrl(node.id).then(({ url }) => setSrc(url));
  }, [node.id]);

  React.useEffect(() => {
    let active = true;
    downloadUrl(node.id)
      .then(({ url }) => active && setSrc(url))
      .catch(() => active && failRef.current({ title: "ファイルを読み込めませんでした", description: "時間をおいてもう一度開くか、ダウンロードしてください。" }));
    return () => {
      active = false;
    };
  }, [node.id]);

  useFileChrome(node, onDownload, {}, []);

  const unsupported = {
    description: `この${kind === "video" ? "動画" : "音声"}の形式はブラウザで再生できません。ダウンロードして、お使いのアプリで開いてください。`,
  };
  const onError = () => {
    const el = mediaRef.current;
    if (!playable.current || !el) {
      failRef.current(unsupported);
      return;
    }
    // 期限切れの URL を取り直して、同じ位置から続ける（1 回だけ。続けて失敗したら諦める）。
    playable.current = false;
    resume.current = { time: el.currentTime, playing: !el.paused };
    load().catch(() => failRef.current(unsupported));
  };
  const onLoadedMetadata = () => {
    const el = mediaRef.current;
    playable.current = true;
    const r = resume.current;
    if (el && r) {
      resume.current = null;
      el.currentTime = r.time;
      if (r.playing) void el.play().catch(() => undefined);
    }
  };

  if (!src) return <EditorLoading kind="doc" message="ファイルを開いています…" />;
  if (kind === "video") {
    return (
      <div className="flex h-full min-h-0 items-center justify-center bg-muted/40 p-0 md:p-6" data-testid="video-preview">
        <video
          ref={(el) => {
            mediaRef.current = el;
          }}
          src={src}
          controls
          playsInline
          preload="metadata"
          onError={onError}
          onLoadedMetadata={onLoadedMetadata}
          className="max-h-full max-w-full bg-black md:rounded-xl md:shadow-[0_1px_2px_rgb(0_0_0/0.06),0_16px_40px_-12px_rgb(0_0_0/0.3)] md:ring-1 md:ring-black/10"
          data-testid="video-preview-media"
        />
      </div>
    );
  }
  return (
    <div className="flex h-full min-h-0 items-center justify-center overflow-auto bg-muted/40 p-4 sm:p-6" data-testid="audio-preview">
      <AudioPlayer
        src={src}
        title={node.name}
        meta={[nodeKindLabel(node), formatBytes(node.size_bytes)].filter(Boolean).join("・")}
        mediaRef={(el) => {
          mediaRef.current = el;
        }}
        onError={onError}
        onLoadedMetadata={onLoadedMetadata}
      />
    </div>
  );
}
