"use client";

import * as React from "react";

import { artifactPages, type ArtifactKind, type ArtifactMeta } from "@/lib/artifact-api";
import { useOwnedSharedList, type OwnedSharedListState } from "@/hooks/use-owned-shared-list";

/// スキル・アプリなどの一覧の状態（[`useArtifactList`]）。
export type ArtifactListState = OwnedSharedListState<ArtifactMeta>;

/// アーティファクトの一覧を読み、自分のものと共有された分の続きを押して読めるようにする（BUG-162・BUG-186）。
///
/// 自分のものは 50 件ずつ、共有された分は続きの位置をたどって読む。読み切れなかったことと、続きの頼みの失敗は、
/// 画面が「さらに読み込む」（[`ListMore`]）で言う（黙って欠かない）。
export function useArtifactList(kind: ArtifactKind, options?: { enabled?: boolean }): ArtifactListState {
  const fetchPage = React.useMemo(() => artifactPages(kind), [kind]);
  return useOwnedSharedList(fetchPage, options);
}
