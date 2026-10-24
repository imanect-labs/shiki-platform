/// CSV 取込の BFF 中継（A P3）。
///
/// rewrites のプロキシは、サーバが本文の大きさだけ見て 413 を返し接続を閉じると、残りの本文を書けずに
/// （EPIPE）**500** を返していた。ここでは本文の大きさ（Content-Length）がサーバの上限を超えるなら上流へ
/// 送らずに、サーバと同じ形の 413 `{code:"import_too_large", max_bytes}` を返す。上限以内はそのまま中継する
/// （上限・検証の正はサーバ。ここの上限は送る前に断るためだけの写し・`MAX_IMPORT_BYTES`）。

import { isGatewayFailure, upstreamUnavailable } from "@/lib/bff-upstream";
import { MAX_IMPORT_BYTES } from "@/lib/data-api";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

const BACKEND = process.env.BACKEND_ORIGIN ?? "http://localhost:8080";
const HOP = ["host", "connection", "keep-alive", "transfer-encoding", "upgrade", "accept-encoding"];

function tooLarge(): Response {
  return Response.json(
    { status: 413, title: "Payload Too Large", code: "import_too_large", max_bytes: MAX_IMPORT_BYTES },
    { status: 413 },
  );
}

export async function POST(req: Request, ctx: { params: Promise<{ id: string }> }): Promise<Response> {
  const { id } = await ctx.params;
  const length = Number(req.headers.get("content-length") ?? "0");
  if (Number.isFinite(length) && length > MAX_IMPORT_BYTES) return tooLarge();
  const headers = new Headers(req.headers);
  for (const h of HOP) headers.delete(h);
  const search = new URL(req.url).search;
  try {
    const upstream = await fetch(`${BACKEND}/data/tables/${encodeURIComponent(id)}/imports/csv${search}`, {
      method: "POST",
      headers,
      body: req.body,
      // 本文を流したまま送る（全体を手元に溜めない）。
      duplex: "half",
      redirect: "manual",
      cache: "no-store",
    } as RequestInit & { duplex: "half" });
    if (isGatewayFailure(upstream)) return upstreamUnavailable();
    const out = new Headers();
    for (const h of ["content-type", "location", "retry-after"]) {
      const v = upstream.headers.get(h);
      if (v) out.set(h, v);
    }
    return new Response(upstream.body, { status: upstream.status, headers: out });
  } catch {
    // 長さの分からない本文（chunked）で上限を超え、サーバが先に閉じたときもここに来る。長さが分かっていて上限
    // 以内なら、上流に届かなかった（停止・再起動中・3-B-1）。
    if (req.signal.aborted) return new Response(null, { status: 499 });
    return length === 0 ? tooLarge() : upstreamUnavailable();
  }
}
