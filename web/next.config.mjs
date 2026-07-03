/** @type {import('next').NextConfig} */

// BFF 同一オリジン化: ブラウザは web(:3000) だけを見て、API/認証はサーバ側で
// shiki-server へプロキシする。これでセッション Cookie が first-party になり、
// CORS + credential の複雑さを避けられる（docs/auth/browser-token-strategy.md §7.1）。
// 本番で BACKEND_ORIGIN の設定漏れがあると全 API/認証が localhost:8080 へ静かに流れて
// 失敗するため、フォールバック時は警告を出す（単一ホスト構成では既定値が正なので throw は
// しない — CI/デモの起動やビルドを壊さない）。実運用ではインフラ側で必ず設定すること。
if (!process.env.BACKEND_ORIGIN && process.env.NODE_ENV === "production") {
  console.warn(
    "[next.config] BACKEND_ORIGIN が未設定です。http://localhost:8080 にフォールバックします。" +
      "単一ホスト以外の本番では必ず BACKEND_ORIGIN を設定してください。",
  );
}
const backendOrigin = process.env.BACKEND_ORIGIN ?? "http://localhost:8080";

const nextConfig = {
  reactStrictMode: true,
  // SSE（チャット応答）を逐次フラッシュするため gzip 圧縮を無効化する。
  // Next 既定の圧縮は text/event-stream をバッファし「1文字ずつ出ない」原因になる。
  compress: false,
  async rewrites() {
    return [
      // フロントの API 呼び出し（/api/me 等）→ shiki-server のルート（/me 等）。
      { source: "/api/:path*", destination: `${backendOrigin}/:path*` },
      // BFF 認証エンドポイント（/auth/login・/auth/callback・/auth/logout・/auth/session）。
      { source: "/auth/:path*", destination: `${backendOrigin}/auth/:path*` },
    ];
  },
};

export default nextConfig;
