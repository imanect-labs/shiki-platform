import { NextResponse, type NextRequest } from "next/server";

/// セッション Cookie 名。サーバ定数 `crate::session::SESSION_COOKIE`（"shiki_session"）と
/// 一致させる契約。サーバ側も固定値のためドリフトしない（api.ts の CSRF_COOKIE と同方針）。
const SESSION_COOKIE = "shiki_session";

/// 保護ルートの第一関門（サーバ側）。セッション Cookie が無ければログイン画面へ誘導する。
/// Cookie は httpOnly のため JS からは読めないが、middleware はサーバで動くので存在を確認できる。
/// Cookie の「有効性」までは検証しない（失効済みは後段で /me が 401 → client が /login へ誘導）。
/// 二段構え（cookie 有無 ＝ middleware / 実効性 ＝ /me）で UX と厳密性を両立する。
export function middleware(req: NextRequest) {
  if (req.cookies.has(SESSION_COOKIE)) return NextResponse.next();

  const url = req.nextUrl.clone();
  const next = req.nextUrl.pathname + req.nextUrl.search;
  url.pathname = "/login";
  url.search = "";
  if (next && next !== "/") url.searchParams.set("next", next);
  return NextResponse.redirect(url);
}

export const config = {
  // 保護対象から除外: /login・/auth/*（BFF）・/api/*（プロキシ）・
  // Next 内部アセット・拡張子付き静的ファイル。それ以外（/・/c/*・/drive*・/settings）を保護する。
  matcher: [
    "/((?!login|auth|api|_next/static|_next/image|favicon.ico|.*\\..*).*)",
  ],
};
