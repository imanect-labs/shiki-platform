/// 業務データの変更の購読（SSE）の BFF 中継（D-2）。
///
/// `/api/*` は next.config.mjs の rewrites で shiki-server へ素通しするが、rewrites のプロキシは**上流の切断を
/// ブラウザへ伝えない**（サーバの再起動・停止で上流が閉じても、ブラウザ側の接続は開いたまま黙る）。その間
/// EventSource は error を受け取らず、Last-Event-ID 付きの繋ぎ直しも起きないので、グリッドの自動反映が黙って
/// 止まっていた。このルート（app のルートは rewrites より先に解決される）で購読だけを中継し、
/// - 上流が閉じたら／読めなくなったら、ブラウザ側も閉じる（EventSource が繋ぎ直す）
/// - 上流から一定時間なにも届かない（サーバは 15 秒ごとに keepalive のコメント行を送る）ときは、上流が
///   半開きとみなして閉じる
/// - ブラウザが離れたら上流も閉じる
/// 中継するのはバイト列そのまま（イベントの中身は読まない・書き換えない）。認可はすべて shiki-server 側。
/// 200 以外（400 `cursor_invalid`・404 など）は状態・本文をそのまま返し、上流に届かないときは他の中継と同じ
/// 503 ＋ `Retry-After` ＋構造化した本文を返す（3-B-1）。EventSource は 200 以外で諦めるので、画面は REST で
/// 原因を確かめて繋ぎ直す（data-grid の diagnoseStream）。

import { relaySse } from "@/lib/bff-sse";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

export async function GET(req: Request, ctx: { params: Promise<{ id: string }> }): Promise<Response> {
  const { id } = await ctx.params;
  return relaySse(req, `/data/tables/${encodeURIComponent(id)}/stream`);
}
