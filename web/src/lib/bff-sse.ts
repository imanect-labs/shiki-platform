/// SSE の購読の BFF 中継（`app/api/**/stream/route.ts` が共有する）。
///
/// `/api/*` の素通し（rewrites）のプロキシは**上流の切断をブラウザへ伝えない**（サーバの再起動・停止で上流が
/// 閉じても、ブラウザ側の接続は開いたまま黙る）。その間 EventSource は error を受け取らず、繋ぎ直しも
/// 起きない。購読はこの関数で中継し、
/// - 上流が閉じたら／読めなくなったら、ブラウザ側も閉じる（EventSource が繋ぎ直す）
/// - 上流から一定時間なにも届かない（サーバは 15 秒ごとに keepalive を送る）ときは、上流が半開きとみなして閉じる
/// - ブラウザが離れたら上流も閉じる
/// 中継するのはバイト列そのまま（イベントの中身は読まない・書き換えない）。認可はすべて shiki-server 側。
/// 200 以外は状態・本文をそのまま返し、上流に届かないときは他の中継と同じ 503 ＋ `Retry-After` ＋構造化した
/// 本文を返す（3-B-1）。

import { isGatewayFailure, upstreamUnavailable } from "@/lib/bff-upstream";

const BACKEND = process.env.BACKEND_ORIGIN ?? "http://localhost:8080";
/// サーバの keepalive（15 秒）の 3 回分なにも届かなければ上流は死んでいるとみなす。
const IDLE_MS = 45_000;
/// 上流へ渡さないヘッダ（接続ごとのもの・宛先が変わるもの）。
const HOP = ["host", "connection", "keep-alive", "transfer-encoding", "upgrade", "content-length", "accept-encoding"];

/// `upstreamPath`（shiki-server 側のパス・`/api` を外したもの）の SSE を中継する。
export async function relaySse(req: Request, upstreamPath: string): Promise<Response> {
  const search = new URL(req.url).search;
  const headers = new Headers(req.headers);
  for (const h of HOP) headers.delete(h);

  const abort = new AbortController();
  const onClientGone = () => abort.abort();
  req.signal.addEventListener("abort", onClientGone);

  let upstream: Response;
  try {
    upstream = await fetch(`${BACKEND}${upstreamPath}${search}`, {
      headers,
      signal: abort.signal,
      cache: "no-store",
      redirect: "manual",
    });
  } catch {
    req.signal.removeEventListener("abort", onClientGone);
    // ブラウザが離れた（中止）なら返す先は無い。
    if (req.signal.aborted) return new Response(null, { status: 499 });
    // 上流に繋がらない（サーバの停止・再起動中）。
    return upstreamUnavailable();
  }

  const outHeaders = new Headers();
  for (const h of ["content-type", "www-authenticate", "retry-after"]) {
    const v = upstream.headers.get(h);
    if (v) outHeaders.set(h, v);
  }
  // 圧縮・中間でのバッファリングをさせない（イベントを即時に届ける）。
  outHeaders.set("cache-control", "no-cache, no-transform");
  outHeaders.set("x-accel-buffering", "no");

  if (!upstream.ok || !upstream.body) {
    req.signal.removeEventListener("abort", onClientGone);
    if (isGatewayFailure(upstream)) {
      abort.abort();
      return upstreamUnavailable();
    }
    // 拒否の理由（構造化した本文）をそのまま返す。本文は短いので読み切ってから上流を閉じる。
    let text: string | null = null;
    try {
      text = upstream.body ? await upstream.text() : null;
    } catch {
      text = null;
    }
    abort.abort();
    outHeaders.set("cache-control", "no-store");
    outHeaders.delete("x-accel-buffering");
    return new Response(text, { status: upstream.status, headers: outHeaders });
  }

  const reader = upstream.body.getReader();
  let idle: ReturnType<typeof setTimeout> | null = null;
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      const finish = (err?: unknown) => {
        if (idle) clearTimeout(idle);
        idle = null;
        req.signal.removeEventListener("abort", onClientGone);
        abort.abort();
        try {
          if (err) controller.error(err);
          else controller.close();
        } catch {
          // 既に閉じている。
        }
      };
      const arm = () => {
        if (idle) clearTimeout(idle);
        idle = setTimeout(() => finish(new Error("upstream idle")), IDLE_MS);
      };
      arm();
      void (async () => {
        try {
          for (;;) {
            const { done, value } = await reader.read();
            if (done) break;
            arm();
            controller.enqueue(value);
          }
          finish();
        } catch (e) {
          finish(e);
        }
      })();
    },
    cancel() {
      if (idle) clearTimeout(idle);
      req.signal.removeEventListener("abort", onClientGone);
      abort.abort();
    },
  });
  return new Response(body, { status: upstream.status, headers: outHeaders });
}
