import type { Metadata } from "next";
import { Suspense } from "react";

import { MessagesWorkspace } from "@/components/messages/messages-workspace";

/// 職員間メッセージ（Phase 14・design §4.14）。チャンネル・DM・スレッド・検索・メンション。
export const metadata: Metadata = { title: "メッセージ" };

export default function MessagesPage() {
  // 開いているチャンネル・スレッドを URL（useSearchParams）に持つので Suspense で包む。
  return (
    <Suspense>
      <MessagesWorkspace />
    </Suspense>
  );
}
