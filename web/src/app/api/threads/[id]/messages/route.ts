// チャット SSE 専用のストリーミングプロキシ。
//
// Next の `rewrites`（next.config.mjs）は外部オリジンへのプロキシ時に応答を**バッファ**し、
// SSE が最後まで溜まってから一括で届く（＝1文字ずつ出ない・thinking が流れない）。
// この Route Handler は filesystem route として rewrite より優先され、backend の
// `res.body`(ReadableStream) をそのまま返すことで**逐次フラッシュ**を保つ。
import type { NextRequest } from "next/server";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const BACKEND = process.env.BACKEND_ORIGIN ?? "http://localhost:8080";

export async function POST(req: NextRequest, ctx: { params: Promise<{ id: string }> }) {
  const { id } = await ctx.params;
  const upstream = await fetch(`${BACKEND}/threads/${id}/messages`, {
    method: "POST",
    headers: {
      "content-type": req.headers.get("content-type") ?? "application/json",
      // 同一オリジン Cookie（セッション）と double-submit CSRF を素通しする。
      cookie: req.headers.get("cookie") ?? "",
      "x-csrf-token": req.headers.get("x-csrf-token") ?? "",
    },
    body: await req.text(),
    cache: "no-store",
  });

  // backend のストリームを素通し。圧縮/バッファを抑止するヘッダを付ける。
  return new Response(upstream.body, {
    status: upstream.status,
    headers: {
      "content-type": upstream.headers.get("content-type") ?? "text/event-stream; charset=utf-8",
      "cache-control": "no-cache, no-transform",
      "x-accel-buffering": "no",
    },
  });
}
