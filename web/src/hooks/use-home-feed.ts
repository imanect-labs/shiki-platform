import * as React from "react";

import { listThreads, type Thread } from "@/lib/chat-api";
import {
  fetchMyUsage,
  fetchPendingApprovals,
  fetchWorkflowRunDigest,
  type MyUsage,
  type PendingApproval,
  type WorkflowRunDigest,
} from "@/lib/home-today";
import { sharedWithMe, type NodeResponse } from "@/lib/storage";

/// ホームの「続きから」と「最近の共有」に出す件数。最初から必要な件数だけ取る（全件取得しない）。
export const HOME_FEED_LIMIT = 4;

/// 共有は少し多めに取る。一覧に出すのは先頭 HOME_FEED_LIMIT 件だが、候補プロンプトは文書（ファイル）
/// から作るので、新しい共有がフォルダばかりでも、その後ろの文書を候補に使えるようにする。
const SHARES_FETCH_LIMIT = 12;

export type HomeFeed = {
  /// null は読み込み中。
  threads: Thread[] | null;
  /// 一覧に出す最近の共有（フォルダを含む先頭 HOME_FEED_LIMIT 件）。
  shares: NodeResponse[] | null;
  /// 候補に使う、最近共有された文書（フォルダを除く・新しい順）。
  sharedFiles: NodeResponse[] | null;
  /// 自分の承認を待っている run（#965）。
  approvals: PendingApproval[] | null;
  /// 直近 24 時間のワークフロー実行の集計（取れなければ空の集計）。
  workflows: WorkflowRunDigest | null;
  /// 本人の当月の利用額と上限（LLM 未構成・取得失敗は null のまま＝出さない）。
  usage: MyUsage | null;
  /// 「今日のこと」の 3 つを読み終えたか（読み込み中の骨組みに使う）。
  todayLoaded: boolean;
};

const EMPTY_DIGEST: WorkflowRunDigest = {
  window_hours: 24,
  succeeded: 0,
  failed: 0,
  recent_failures: [],
};

/// ホームに出す最近の会話・自分に共有されたもの・「今日のこと」を並行で読む。
/// どれかが失敗しても残りは出す（失敗した側は空として扱い、ホームを止めない）。
export function useHomeFeed(): HomeFeed {
  const [feed, setFeed] = React.useState<HomeFeed>({
    threads: null,
    shares: null,
    sharedFiles: null,
    approvals: null,
    workflows: null,
    usage: null,
    todayLoaded: false,
  });

  React.useEffect(() => {
    let active = true;
    listThreads(undefined, { limit: HOME_FEED_LIMIT })
      .then((r) => r.threads.slice(0, HOME_FEED_LIMIT))
      .catch(() => [])
      .then((threads) => active && setFeed((f) => ({ ...f, threads })));
    sharedWithMe({ limit: SHARES_FETCH_LIMIT })
      .then((r) => r.items)
      .catch(() => [])
      .then(
        (items) =>
          active &&
          setFeed((f) => ({
            ...f,
            shares: items.slice(0, HOME_FEED_LIMIT),
            sharedFiles: items.filter((n) => n.kind !== "folder"),
          })),
      );
    Promise.all([
      fetchPendingApprovals().catch(() => [] as PendingApproval[]),
      fetchWorkflowRunDigest().catch(() => EMPTY_DIGEST),
      fetchMyUsage().catch(() => null),
    ]).then(
      ([approvals, workflows, usage]) =>
        active && setFeed((f) => ({ ...f, approvals, workflows, usage, todayLoaded: true })),
    );
    return () => {
      active = false;
    };
  }, []);

  return feed;
}
