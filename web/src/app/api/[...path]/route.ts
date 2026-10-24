/// `/api/*` の BFF 中継（shiki-server の `/*` へ素通しする・2-D-P3-6）。
///
/// next.config.mjs の rewrites の素通しは、shiki-server に届かない（停止・再起動中）と本文なしの **500** を返し、
/// 画面は「サーバーエラー（500）」としか言えなかった。このルートで中継し、上流に繋がらないときは
/// **503 ＋ `Retry-After`** と構造化した本文（`code: "upstream_unavailable"`）を返す。届いた応答は状態・本文・
/// ヘッダをそのまま返す（認可・検証はすべて shiki-server 側。中身は読まない・書き換えない）。
///
/// 個別の中継（購読の SSE・CSV 取込）は、より具体的なルートとして先に解決される。

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

const BACKEND = process.env.BACKEND_ORIGIN ?? "http://localhost:8080";
/// 上流へ渡さないヘッダ（接続ごとのもの・宛先で変わるもの）。圧縮は上流に任せず、ここで展開した本文を返す。
const HOP_REQ = ["host", "connection", "keep-alive", "transfer-encoding", "upgrade", "accept-encoding", "content-length"];
/// 返さないヘッダ（fetch が本文を展開済みなので長さ・符号化は付け直させる）。
const HOP_RES = ["connection", "keep-alive", "transfer-encoding", "upgrade", "content-encoding", "content-length", "set-cookie"];
/// 上流に繋がらないときの再試行の目安（秒）。サーバの再起動は 10 秒前後で終わる。
const RETRY_AFTER_SEC = 5;

function unavailable(): Response {
  return Response.json(
    {
      status: 503,
      title: "Service Unavailable",
      code: "upstream_unavailable",
      message: "サーバーに接続できません。少し待ってから再試行してください",
    },
    { status: 503, headers: { "retry-after": String(RETRY_AFTER_SEC), "cache-control": "no-store" } },
  );
}

async function proxy(req: Request): Promise<Response> {
  const url = new URL(req.url);
  // `/api` を外した残り（エンコードはブラウザが送ったまま）。
  const target = `${BACKEND}${url.pathname.replace(/^\/api(?=\/|$)/, "") || "/"}${url.search}`;
  const headers = new Headers(req.headers);
  for (const h of HOP_REQ) headers.delete(h);
  const hasBody = req.method !== "GET" && req.method !== "HEAD";
  let upstream: Response;
  try {
    upstream = await fetch(target, {
      method: req.method,
      headers,
      body: hasBody ? req.body : undefined,
      // 本文を流したまま送る（大きなアップロードを手元に溜めない）。
      ...(hasBody ? { duplex: "half" } : {}),
      redirect: "manual",
      cache: "no-store",
      signal: req.signal,
    } as RequestInit & { duplex?: "half" });
  } catch (e) {
    // ブラウザが離れた（中止）なら返す先は無い。
    if (req.signal.aborted) return new Response(null, { status: 499 });
    void e;
    return unavailable();
  }
  const out = new Headers();
  upstream.headers.forEach((v, k) => {
    if (!HOP_RES.includes(k.toLowerCase())) out.set(k, v);
  });
  for (const c of upstream.headers.getSetCookie()) out.append("set-cookie", c);
  // 上流の手前の経路（ロードバランサ等）の 502/504 も、届かなかったものとして同じ案内にする。
  if ((upstream.status === 502 || upstream.status === 504) && !upstream.headers.get("content-type")?.includes("json")) {
    return unavailable();
  }
  return new Response(req.method === "HEAD" ? null : upstream.body, {
    status: upstream.status,
    statusText: upstream.statusText,
    headers: out,
  });
}

export const GET = proxy;
export const HEAD = proxy;
export const POST = proxy;
export const PUT = proxy;
export const PATCH = proxy;
export const DELETE = proxy;
export const OPTIONS = proxy;
