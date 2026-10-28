/// BFF の中継（`app/api/**/route.ts`）が上流（shiki-server）に届かないときの応答。
///
/// 停止・再起動中の上流に繋がらないと、素通しの rewrites は本文なしの 500・個別の中継は本文なしの 502 を返し、
/// 画面は理由を言えなかった。どの中継も **503 ＋ `Retry-After`** と構造化した本文（`code: "upstream_unavailable"`・
/// サーバのエラー本文と同じ形）で返す（2-D-P3-6・3-B-1）。

/// 上流に繋がらないときの再試行の目安（秒）。サーバの再起動は 10 秒前後で終わる。
export const UPSTREAM_RETRY_AFTER_SEC = 5;

export function upstreamUnavailable(): Response {
  return Response.json(
    {
      status: 503,
      title: "Service Unavailable",
      code: "upstream_unavailable",
      message: "サーバーに接続できません。少し待ってから再試行してください",
    },
    { status: 503, headers: { "retry-after": String(UPSTREAM_RETRY_AFTER_SEC), "cache-control": "no-store" } },
  );
}

/// 上流の手前の経路（ロードバランサ等）が返した 502/504（JSON の本文を持たない）か。届かなかったものとして扱う。
export function isGatewayFailure(upstream: Response): boolean {
  return (upstream.status === 502 || upstream.status === 504) && !upstream.headers.get("content-type")?.includes("json");
}
