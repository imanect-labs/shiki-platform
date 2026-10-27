import * as React from "react";

import { listThreads, type Thread } from "@/lib/chat-api";
import { sharedWithMe, type NodeResponse } from "@/lib/storage";

/// ホームの「続きから」と「最近の共有」に出す件数。最初から必要な件数だけ取る（全件取得しない）。
export const HOME_FEED_LIMIT = 4;

export type HomeFeed = {
  /// null は読み込み中。
  threads: Thread[] | null;
  shares: NodeResponse[] | null;
};

/// ホームに出す最近の会話と、自分に共有されたものを並行で読む。
/// 片方が失敗しても、もう片方は出す（失敗した側は空として扱い、ホームを止めない）。
export function useHomeFeed(): HomeFeed {
  const [feed, setFeed] = React.useState<HomeFeed>({ threads: null, shares: null });

  React.useEffect(() => {
    let active = true;
    listThreads(undefined, { limit: HOME_FEED_LIMIT })
      .then((r) => r.threads.slice(0, HOME_FEED_LIMIT))
      .catch(() => [])
      .then((threads) => active && setFeed((f) => ({ ...f, threads })));
    sharedWithMe({ limit: HOME_FEED_LIMIT })
      .then((r) => r.items.slice(0, HOME_FEED_LIMIT))
      .catch(() => [])
      .then((shares) => active && setFeed((f) => ({ ...f, shares })));
    return () => {
      active = false;
    };
  }, []);

  return feed;
}
