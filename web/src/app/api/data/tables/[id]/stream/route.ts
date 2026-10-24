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

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

const BACKEND = process.env.BACKEND_ORIGIN ?? "http://localhost:8080";
/// サーバの keepalive（15 秒）の 3 回分なにも届かなければ上流は死んでいるとみなす。
const IDLE_MS = 45_000;
/// 上流へ渡さないヘッダ（接続ごとのもの・宛先が変わるもの）。
const HOP = ["host", "connection", "keep-alive", "transfer-encoding", "upgrade", "content-length", "accept-encoding"];

export async function GET(req: Request, ctx: { params: Promise<{ id: string }> }): Promise<Response> {
  const { id } = await ctx.params;
  const search = new URL(req.url).search;
  const headers = new Headers(req.headers);
  for (const h of HOP) headers.delete(h);

  const abort = new AbortController();
  const onClientGone = () => abort.abort();
  req.signal.addEventListener("abort", onClientGone);

  let upstream: Response;
  try {
    upstream = await fetch(`${BACKEND}/data/tables/${encodeURIComponent(id)}/stream${search}`, {
      headers,
      signal: abort.signal,
      cache: "no-store",
      redirect: "manual",
    });
  } catch {
    req.signal.removeEventListener("abort", onClientGone);
    // 上流に繋がらない（サーバの停止・再起動中）。EventSource は 200 以外で諦めるので、呼び出し側が
    // REST で原因を確かめて繋ぎ直す（data-grid の diagnoseStream）。
    return new Response(null, { status: 502 });
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
    abort.abort();
    return new Response(null, { status: upstream.status, headers: outHeaders });
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
