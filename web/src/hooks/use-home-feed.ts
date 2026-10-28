import * as React from "react";

import { listThreads, type Thread } from "@/lib/chat-api";
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
};

/// ホームに出す最近の会話と、自分に共有されたものを並行で読む。
/// 片方が失敗しても、もう片方は出す（失敗した側は空として扱い、ホームを止めない）。
export function useHomeFeed(): HomeFeed {
  const [feed, setFeed] = React.useState<HomeFeed>({
    threads: null,
    shares: null,
    sharedFiles: null,
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
    return () => {
      active = false;
    };
  }, []);

  return feed;
}
