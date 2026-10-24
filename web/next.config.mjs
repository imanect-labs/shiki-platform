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
  // 出力先の差し替え（既定は .next）。起動中の本番ビルドを壊さずに別ポートで dev/build を並走させるときだけ
  // NEXT_DIST_DIR=.next-xxx を渡す（.gitignore・eslint・tsconfig は .next-* を除外済み）。
  distDir: process.env.NEXT_DIST_DIR || ".next",
  // コンテナ用の自己完結出力。node_modules 全体ではなく実際に使うものだけを
  // .next/standalone へまとめるので、イメージが数百 MB 小さくなる。
  // **サーバは `node server.js` で起動する**（`next start` ではない）。
  output: "standalone",
  async rewrites() {
    // fallback: app の route handler（動的な `[id]` を含む）を先に解決し、無いものだけを素通しする。
    // 配列で返す既定（afterFiles）は動的ルートより先に効くため、/api/data/tables/[id]/stream などの
    // 中継（上流の切断を伝える・大きな取込を 413 で断る）が素通しに奪われていた。
    return {
      // WebSocket（共同編集の /api/collab/docs/{id}/ws）は route handler では受けられない。catch-all の
      // /api/[...path] が先に一致すると upgrade が切られるので、ファイルより先に素通しする。
      beforeFiles: [{ source: "/api/collab/:path*", destination: `${backendOrigin}/collab/:path*` }],
      fallback: [
        // フロントの API 呼び出し（/api/me 等）→ shiki-server のルート（/me 等）。
        { source: "/api/:path*", destination: `${backendOrigin}/:path*` },
        // BFF 認証エンドポイント（/auth/login・/auth/callback・/auth/logout・/auth/session）。
        { source: "/auth/:path*", destination: `${backendOrigin}/auth/:path*` },
      ],
    };
  },
};

export default nextConfig;
