//! 組み込み砂箱バンドル配信（Task 11.2・第3リスナ＝apps オリジンに同居）。
//!
//! `GET /builtin/{name}` で**プラットフォーム同梱**の self-contained HTML
//! （スライドエディタ等）を配信する。B1 のユーザー供給バンドル（`/a/...`）と違い、
//! 内容はリリース成果物そのもの＝信頼の根はデプロイにある（content-address ピンは不要）。
//! 隔離は「**apps オリジン＝アプリ本体と別オリジン**＋**通信の全遮断 CSP**」で、アプリ本体の
//! DOM/cookie/API へ同一オリジンポリシーで到達できない（design §4.8.3・PIT-40 第4層・
//! opaque origin にしない理由は [`builtin_csp`] の rustdoc 参照）。
//!
//! 配信元はローカルディレクトリ（`SHIKI__GATEWAY__BUILTIN_DIR`）。実行時の外部
//! ダウンロードはしない（PIT-33 と同型・エアギャップ配布可）。

use std::path::PathBuf;

use axum::{
    extract::{Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};

/// 組み込みバンドル配信の共有状態。
#[derive(Clone)]
pub struct BuiltinState {
    /// バンドル HTML の配置ディレクトリ（デプロイ成果物）。
    pub dir: PathBuf,
    /// CSP `frame-ancestors` に許可するホスト（web シェル）のオリジン。
    pub host_origin: String,
}

/// 許可する組み込みバンドル名の閉集合（パス注入をアーキテクチャ的に排除）。
fn builtin_file(name: &str) -> Option<&'static str> {
    match name {
        "slide-editor" => Some("slide-editor.html"),
        _ => None,
    }
}

/// 組み込みバンドルの CSP（純粋関数・golden 単体テスト対象）。
///
/// B1 の [`crate::bundle_csp`] と 2 点で異なる:
/// - **`sandbox` ディレクティブを使わない**（opaque origin にしない）。GrapesJS は
///   自身のキャンバス iframe へ同一オリジンで触る必要があり、opaque origin では
///   `contentDocument` が null になり動かない。組み込みバンドルは**プラットフォーム
///   同梱の信頼済みコード**（ユーザー供給ではない）ため、隔離は「apps オリジン＝
///   アプリ本体と別オリジン」＋「通信の全遮断」で担保する（アプリの DOM/cookie には
///   同一オリジンポリシーで到達できない・データ持ち出し経路は CSP で無い）。
///   ユーザー供給の B1 バンドルは従来どおり opaque origin（[`crate::bundle_csp`]）。
/// - エディタは通信を一切持たないため `connect-src` を許可しない（`default-src 'none'`）。
pub fn builtin_csp(host_origin: &str) -> String {
    format!(
        "default-src 'none'; \
         script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
         img-src data: blob:; font-src data:; frame-src data: about:; \
         frame-ancestors {host_origin}"
    )
}

/// 組み込みバンドル Router（`/builtin/{name}`・B1 リスナへ merge して同居させる）。
pub fn build_builtin_router(state: BuiltinState) -> Router {
    Router::new()
        .route("/builtin/{name}", get(serve_builtin))
        .with_state(state)
}

async fn serve_builtin(State(state): State<BuiltinState>, Path(name): Path<String>) -> Response {
    let Some(file) = builtin_file(&name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let path = state.dir.join(file);
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) => {
            // 未配備（ビルドしていない開発環境等）は 404＝機能 off として fail-closed。
            tracing::warn!(path = %path.display(), error = %e, "組み込みバンドルが読めません");
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    let csp = builtin_csp(&state.host_origin);
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_str(&csp).unwrap_or_else(|_| HeaderValue::from_static("sandbox")),
            ),
            // リリースごとに中身が変わり得るため immutable にしない（再検証させる）。
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
        ],
        bytes,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::{builtin_csp, builtin_file};

    /// CSP golden（受け入れ条件: 通信全遮断・埋め込み先制限・外部リソース遮断）。
    #[test]
    fn csp_golden() {
        let csp = builtin_csp("http://host.example:3000");
        assert_eq!(
            csp,
            "default-src 'none'; \
             script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
             img-src data: blob:; font-src data:; frame-src data: about:; \
             frame-ancestors http://host.example:3000"
        );
        // 変更検知の要点: connect-src が無い（default-src 'none' が通信を全遮断）・
        // 埋め込み先はホストシェルのみ。
        assert!(!csp.contains("connect-src"));
        assert!(csp.contains("frame-ancestors http://host.example:3000"));
    }

    #[test]
    fn 未知バンドル名は拒否() {
        assert_eq!(builtin_file("slide-editor"), Some("slide-editor.html"));
        assert_eq!(builtin_file("../etc/passwd"), None);
        assert_eq!(builtin_file("other"), None);
    }
}
