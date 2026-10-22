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
  // URL を取り直したか。取り直した後に再生が先へ進んだら、また取り直してよい。
  const retried = React.useRef(false);
  const resumedAt = React.useRef(0);
  const onTimeUpdate = () => {
    const el = mediaRef.current;
    if (retried.current && el && el.currentTime > resumedAt.current + 1) retried.current = false;
  };
  const failRef = React.useRef(onUnsupported);
  failRef.current = onUnsupported;
  // 画面を離れた後に、取り直しの結果（成功・失敗とも）を反映しない（別のファイルの画面を壊さない）。
  const mounted = React.useRef(true);
  React.useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const load = React.useCallback(() => {
    return downloadUrl(node.id).then(({ url }) => {
      if (mounted.current) setSrc(url);
    });
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
    // 期限切れの URL を取り直して、同じ位置から続ける。取り直した後に再生が進まないまま
    // また失敗したら（ファイルの途中が壊れているなど）諦める（取り直しを繰り返さない）。
    if (retried.current) {
      failRef.current(unsupported);
      return;
    }
    retried.current = true;
    playable.current = false;
    resume.current = { time: el.currentTime, playing: !el.paused };
    load().catch(() => {
      if (mounted.current) failRef.current(unsupported);
    });
  };
  const onLoadedMetadata = () => {
    const el = mediaRef.current;
    playable.current = true;
    const r = resume.current;
    if (el && r) {
      resume.current = null;
      resumedAt.current = r.time;
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
          onTimeUpdate={onTimeUpdate}
          className="max-h-full max-w-full bg-black md:rounded-xl md:shadow-[0_1px_2px_rgb(0_0_0/0.06),0_16px_40px_-12px_rgb(0_0_0/0.3)] md:ring-1 md:ring-black/10"
          data-testid="video-preview-media"
        />
      </div>
    );
  }
  return (
    <div className="flex h-full min-h-0 items-start justify-center overflow-auto bg-muted/40 p-4 pt-6 sm:items-center sm:p-6" data-testid="audio-preview">
      <AudioPlayer
        src={src}
        title={node.name}
        meta={[nodeKindLabel(node), formatBytes(node.size_bytes)].filter(Boolean).join("・")}
        mediaRef={(el) => {
          mediaRef.current = el;
        }}
        onError={onError}
        onLoadedMetadata={onLoadedMetadata}
        onTimeUpdate={onTimeUpdate}
      />
    </div>
  );
}
