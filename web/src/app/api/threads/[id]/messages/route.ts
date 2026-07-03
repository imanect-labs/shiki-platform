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
  const body = await req.text();

  let upstream: Response;
  try {
    upstream = await fetch(`${BACKEND}/threads/${id}/messages`, {
      method: "POST",
      headers: {
        "content-type": req.headers.get("content-type") ?? "application/json",
        // 同一オリジン Cookie（セッション）と double-submit CSRF を素通しする。
        cookie: req.headers.get("cookie") ?? "",
        "x-csrf-token": req.headers.get("x-csrf-token") ?? "",
      },
      body,
      cache: "no-store",
      // クライアント切断時に upstream の生成も止める（放置された接続で backend が
      // 生成し続けるのを防ぐ）。req.signal は client disconnect で abort される。
      signal: req.signal,
    });
  } catch (e) {
    // クライアント切断由来の abort は正常系（400 系のノイズを出さない）。
    if (req.signal.aborted) return new Response(null, { status: 499 });
    // backend 到達不可などは構造化した SSE エラーで返し、呼び出し元が判別できるようにする。
    const message = e instanceof Error ? e.message : "バックエンドに接続できませんでした";
    return new Response(`event: error\ndata: ${JSON.stringify({ message })}\n\n`, {
      status: 502,
      headers: { "content-type": "text/event-stream; charset=utf-8", "cache-control": "no-cache" },
    });
  }

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
