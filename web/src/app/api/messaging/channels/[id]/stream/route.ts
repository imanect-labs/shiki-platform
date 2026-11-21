/// 職員間メッセージの配信（SSE）の BFF 中継（Phase 14・design §4.14 配信）。
///
/// catch-all の `/api/[...path]` も本文を流したまま返すが、上流が半開きになったとき・切れたときに
/// ブラウザへ伝えない。購読だけはこのルートで中継し、上流の切断・無通信（keepalive が届かない）を
/// ブラウザ側の切断として伝える（EventSource が繋ぎ直す・中身は読まない）。認可はすべて shiki-server 側。

import { relaySse } from "@/lib/bff-sse";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

export async function GET(req: Request, ctx: { params: Promise<{ id: string }> }): Promise<Response> {
  const { id } = await ctx.params;
  return relaySse(req, `/messaging/channels/${encodeURIComponent(id)}/stream`);
}
