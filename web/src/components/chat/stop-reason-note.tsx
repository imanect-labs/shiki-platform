import { CircleAlert } from "lucide-react";

import type { StopReason } from "@/lib/chat-api";

/// 続きの頼み方。次の発話は前の回答の本文を読んだうえで、手数・費用の上限を数え直して始まる。
const CONTINUE = "続きが必要なら「続けて」と送ってください。";

/// 回答が途中で止まった理由ごとの注記（GAP-106）。理由は Rust の `chat::StopReason`（codegen）。
const NOTES: Record<StopReason, string> = {
  truncated: `回答が長くなったため、途中で打ち切りました。${CONTINUE}`,
  step_limit: `手順の上限に達したため、途中で止めました。${CONTINUE}`,
  time_limit: `時間の上限に達したため、途中で止めました。${CONTINUE}`,
  token_limit: `1 回の回答で読み書きできる量の上限に達したため、途中で止めました。${CONTINUE}`,
  cost_limit: `1 回の回答にかけられる費用の上限に達したため、途中で止めました。${CONTINUE}`,
  loop_detected: "同じ操作の失敗が続いたため、途中で止めました。頼み方を変えて、もう一度送ってください。",
};

/// 途中で止まった回答の下に添える短い注記。理由の無い回答（自然に終わった・止めた）には出さない。
export function StopReasonNote({ reason }: { reason: StopReason | null | undefined }) {
  if (!reason) return null;
  const text = NOTES[reason];
  if (!text) return null;
  return (
    <p
      role="note"
      data-testid="stop-reason-note"
      data-stop-reason={reason}
      className="mt-3 flex items-start gap-2 rounded-lg border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-[12.5px] leading-relaxed text-amber-800 [overflow-wrap:anywhere] dark:text-amber-300"
    >
      <CircleAlert className="mt-0.5 size-3.5 shrink-0" aria-hidden />
      <span className="min-w-0">{text}</span>
    </p>
  );
}
