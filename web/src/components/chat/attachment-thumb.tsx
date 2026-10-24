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

/// 画像として見せる添付か（拡張子で判定・HEIC などブラウザが出せないものは外す）。
export function isImageAttachment(name: string): boolean {
  return previewKind({ name }) === "image";
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
  const [signed, setSigned] = React.useState<string | null>(null);
  const [failed, setFailed] = React.useState(false);

  React.useEffect(() => {
    if (src || !nodeId) return;
    let active = true;
    downloadUrl(nodeId)
      .then(({ url }) => active && setSigned(url))
      .catch(() => active && setFailed(true));
    return () => {
      active = false;
    };
  }, [nodeId, src]);

  const url = src ?? signed;
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
          onError={() => setFailed(true)}
          className={cn("size-full rounded-[inherit] object-cover", imgClassName)}
        />
      ) : (
        <ImageIcon className="size-5 text-muted-foreground" aria-label={name} />
      )}
      {children}
    </span>
  );
}
