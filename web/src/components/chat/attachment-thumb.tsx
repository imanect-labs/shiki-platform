"use client";

/// 画像の添付を小さい四角のサムネイルで見せる（入力欄の添付・送信した発話）。
///
/// 中身は閲覧者本人のセッションで取った署名付き URL で読む（getNode/downloadUrl と同じ ReBAC）。
/// 署名付き URL は node をまたいで使い回さない（キャッシュすると、ログインし直した別の人に
/// 前の人の URL を渡しかねない）。端末から選んだばかりの画像は `src` に object URL を渡す。
import * as React from "react";
import { ImageIcon } from "lucide-react";

import { previewKind } from "@/lib/file-preview";
import { downloadUrl } from "@/lib/storage";
import { cn } from "@/lib/utils";

const NODE_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/// 画像として見せる添付か（拡張子で判定・HEIC などブラウザが出せないものは外す）。
/// 発話の file_ref は他人が書いた値でもあるので、node id が UUID の形のときだけ URL に入れる
/// （任意の文字列を `/files/{id}/download-url` に混ぜて、閲覧者の権限で別のパスを叩かせない）。
export function isImageAttachment(a: { node_id: string; name: string }): boolean {
  return NODE_ID.test(a.node_id) && previewKind({ name: a.name }) === "image";
}

export function AttachmentThumb({
  nodeId,
  name,
  src,
  className,
  imgClassName,
  children,
}: {
  /// `src` が無いときは、この node の署名付き URL を取って出す。
  nodeId?: string;
  name: string;
  src?: string;
  className?: string;
  imgClassName?: string;
  /// 右上の「外す」・アップロード中の表示など、四角の上に重ねるもの。
  children?: React.ReactNode;
}) {
  const [signed, setSigned] = React.useState<{ nodeId: string; url: string } | null>(null);
  // 失敗は「何を読もうとして失敗したか」に結び付ける（画像が替わったら読み直す）。
  const [failedFor, setFailedFor] = React.useState<string | null>(null);

  React.useEffect(() => {
    if (src || !nodeId) return;
    let active = true;
    downloadUrl(nodeId)
      .then(({ url }) => active && setSigned({ nodeId, url }))
      .catch(() => active && setFailedFor(`node:${nodeId}`));
    return () => {
      active = false;
    };
  }, [nodeId, src]);

  const url = src ?? (signed && signed.nodeId === nodeId ? signed.url : null);
  const failed = failedFor !== null && (failedFor === url || failedFor === `node:${nodeId}`);
  return (
    <span
      title={name}
      className={cn(
        "relative inline-flex size-14 shrink-0 items-center justify-center rounded-xl border border-border bg-muted",
        className,
      )}
    >
      {url && !failed ? (
        // 署名付き URL・object URL は next/image の最適化に通せないので素の img で出す。
        // eslint-disable-next-line @next/next/no-img-element
        <img
          src={url}
          alt={name}
          draggable={false}
          decoding="async"
          onError={() => setFailedFor(url)}
          className={cn("size-full rounded-[inherit] object-cover", imgClassName)}
        />
      ) : (
        <ImageIcon role="img" className="size-5 text-muted-foreground" aria-label={name} />
      )}
      {children}
    </span>
  );
}
