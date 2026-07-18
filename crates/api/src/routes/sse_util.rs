//! SSE レスポンス共通ユーティリティ。
//!
//! 接続非依存生成（チャット）やワークフロー実行の SSE は run が開いたまま（承認待ち・進捗）で
//! 長寿命になる。逆プロキシ（Next の BFF rewrite・nginx 等）が SSE を圧縮/バッファすると、
//! 途中のイベントが flush されずブラウザへ live で届かない。中間層の圧縮/バッファを無効化する
//! ヘッダを付けてこれを防ぐ。

use axum::http::{header::CACHE_CONTROL, HeaderName, HeaderValue};
use axum::response::Response;

/// SSE レスポンスに「圧縮/バッファ無効化」ヘッダを付ける。
///
/// - `Cache-Control: no-transform`: 中間プロキシの圧縮（gzip 等）を禁止する（圧縮はバッファを伴う）。
/// - `X-Accel-Buffering: no`: nginx 等にレスポンスバッファリングをしないよう明示する。
pub(crate) fn no_buffer(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    response
}
