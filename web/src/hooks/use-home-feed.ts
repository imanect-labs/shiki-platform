import * as React from "react";

import { useApprovalCountState, type ApprovalCount } from "@/hooks/use-approval-count";
import { listThreads, type Thread } from "@/lib/chat-api";
import {
  fetchMyUsage,
  fetchPendingApprovals,
  fetchWorkflowRunDigest,
  type MyUsage,
  type PendingApproval,
  type WorkflowRunDigest,
} from "@/lib/home-today";
import { sharedWithMeUpTo, type NodeResponse } from "@/lib/storage";

/// ホームの「続きから」と「最近の共有」に出す件数。最初から必要な件数だけ取る（全件取得しない）。
export const HOME_FEED_LIMIT = 4;

/// 共有は少し多めに取る。一覧に出すのは先頭 HOME_FEED_LIMIT 件だが、候補プロンプトは文書（ファイル）
/// から作るので、新しい共有がフォルダばかりでも、その後ろの文書を候補に使えるようにする。
///
/// 共有されたものが多い人には、1 ページ目が短いか空のまま続きの位置だけが付いて返ることがある（BUG-162）。
/// 1 ページ目だけを読むと、共有があっても 1 件も出ないので、ドライブの「共有」の画面と同じく続きをたどって
/// この件数まで集める（往復の上限は sharedWithMeUpTo が持つ）。続きの取得に失敗しても、それまでに集めた分は出す。
const SHARES_FETCH_LIMIT = 12;

export type HomeFeed = {
  /// null は読み込み中。
  threads: Thread[] | null;
  /// 一覧に出す最近の共有（フォルダを含む先頭 HOME_FEED_LIMIT 件）。
  shares: NodeResponse[] | null;
  /// 候補に使う、最近共有された文書（フォルダを除く・新しい順）。
  sharedFiles: NodeResponse[] | null;
  /// 自分の承認を待っている会話の run（チャットのエージェントの承認・#965）。
  approvals: PendingApproval[] | null;
  /// 自分の判断を待っているワークフローの承認の件数（受信箱 /approvals・GAP-08）。読めなければ null（出さない）。
  workflowApprovals: ApprovalCount | null;
  /// 直近 24 時間のワークフロー実行の集計（取れなければ空の集計）。
  workflows: WorkflowRunDigest | null;
  /// 本人の当月の利用額と上限（LLM 未構成・取得失敗は null のまま＝出さない）。
  usage: MyUsage | null;
  /// 「今日のこと」の 4 つ（会話の承認待ち・ワークフローの承認待ち・実行の集計・利用）を読み終えたか（読み込み中の
  /// 骨組みに使う）。
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
///
/// ワークフローの承認待ちの件数は、サイドバーの「承認」のバッジと同じ読み方（[`useApprovalCountState`]・受信箱で
/// 判断したときや 60 秒ごとに読み直す）で読む。
export function useHomeFeed(): HomeFeed {
  const workflowApprovals = useApprovalCountState();
  const [feed, setFeed] = React.useState<Omit<HomeFeed, "workflowApprovals">>({
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
    sharedWithMeUpTo({ want: SHARES_FETCH_LIMIT, keepOnError: true })
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
    // 届いたものから出す（遅い API に承認待ちまで引きずられない）。「読み込み済み」は 3 つとも
    // 終わってから立てる（ワークフローの承認待ちの件数は、読み終えたかを返しで合わせる）。
    const today = [
      fetchPendingApprovals()
        .catch(() => [] as PendingApproval[])
        .then((approvals) => active && setFeed((f) => ({ ...f, approvals }))),
      fetchWorkflowRunDigest()
        .catch(() => EMPTY_DIGEST)
        .then((workflows) => active && setFeed((f) => ({ ...f, workflows }))),
      fetchMyUsage()
        .catch(() => null)
        .then((usage) => active && setFeed((f) => ({ ...f, usage }))),
    ];
    Promise.all(today).then(() => active && setFeed((f) => ({ ...f, todayLoaded: true })));
    return () => {
      active = false;
    };
  }, []);

  return React.useMemo(
    () => ({
      ...feed,
      workflowApprovals: workflowApprovals.value,
      todayLoaded: feed.todayLoaded && workflowApprovals.settled,
    }),
    [feed, workflowApprovals],
  );
}
