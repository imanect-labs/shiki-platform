//! md → .docx 合成（DocxComposer・#332）の結合テスト。
//!
//! worker `/edit` はモック HTTP サーバ（実 TCP・reqwest 経路そのまま）で代替する。
//! DB・ストレージには依存しない（compose は保存しない純変換）。
//!
//! 検証:
//! - 非空 markdown → blank.docx テンプレを base64 で運び append_markdown 1 op を送る
//! - worker の返した bytes がそのまま返る
//! - 422 → Invalid（恒久）、5xx → Worker（一時）に写る

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic
)]

use std::sync::{Arc, Mutex};

use axum::{http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use base64::Engine as _;
use office::{DocxComposer, OfficeError};

/// モック worker が受けたリクエストの記録。
#[derive(Default)]
struct Seen {
    requests: Vec<serde_json::Value>,
}

/// モック worker を実 TCP で起動する。`mode` で応答を切り替える。
async fn spawn_worker(mode: &'static str, seen: Arc<Mutex<Seen>>) -> String {
    let app = Router::new().route(
        "/edit",
        post(move |Json(req): Json<serde_json::Value>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().requests.push(req.clone());
                match mode {
                    "ok" => {
                        let engine = base64::engine::general_purpose::STANDARD;
                        let data = engine.decode(req["data_base64"].as_str().unwrap()).unwrap();
                        let out = [data.as_slice(), b":COMPOSED"].concat();
                        Json(serde_json::json!({
                            "data_base64": engine.encode(out),
                            "report": {
                                "applied_ops": 1,
                                "results": [{ "op": "append_markdown", "applied": 3 }],
                            },
                        }))
                        .into_response()
                    }
                    "noop" => Json(serde_json::json!({
                        "data_base64": req["data_base64"].clone(),
                        "report": { "applied_ops": 0, "results": [] },
                    }))
                    .into_response(),
                    "invalid" => (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        Json(serde_json::json!({
                            "detail": { "error": "unsupported_op", "detail": "未知の op です" }
                        })),
                    )
                        .into_response(),
                    _ => (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response(),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    format!("http://{addr}")
}

/// 非空 markdown はテンプレ＋append_markdown 1 op を worker へ送り、返った bytes をそのまま返す。
#[tokio::test]
async fn compose_sends_template_and_append_markdown() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let base = spawn_worker("ok", seen.clone()).await;
    let composer = DocxComposer::new(reqwest::Client::new(), &base);

    let bytes = composer
        .compose("default", "提案書.docx", "# 提案\n\n- 要点")
        .await
        .expect("compose");
    assert!(String::from_utf8_lossy(&bytes).ends_with(":COMPOSED"));

    let reqs = seen.lock().unwrap();
    assert_eq!(reqs.requests.len(), 1);
    let req = &reqs.requests[0];
    assert_eq!(req["tenant_id"], "default");
    assert_eq!(req["file_name"], "提案書.docx");
    assert_eq!(
        req["content_type"],
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    );
    // 運んだベースは blank.docx テンプレ（zip の PK ヘッダ）。
    let engine = base64::engine::general_purpose::STANDARD;
    let base_bytes = engine.decode(req["data_base64"].as_str().unwrap()).unwrap();
    assert_eq!(&base_bytes[..2], b"PK");
    // op は append_markdown 1 件のみ（office.edit と同契約）。
    assert_eq!(req["ops"].as_array().unwrap().len(), 1);
    assert_eq!(req["ops"][0]["op"], "append_markdown");
    assert_eq!(req["ops"][0]["markdown"], "# 提案\n\n- 要点");
}

/// worker 422（恒久・入力不正）は Invalid に写る。
#[tokio::test]
async fn worker_422_maps_to_invalid() {
    let base = spawn_worker("invalid", Arc::new(Mutex::new(Seen::default()))).await;
    let composer = DocxComposer::new(reqwest::Client::new(), &base);
    let err = composer.compose("default", "x.docx", "本文").await;
    let Err(OfficeError::Invalid(msg)) = err else {
        panic!("Invalid になること: {err:?}");
    };
    assert!(msg.contains("unsupported_op"));
}

/// worker 5xx（一時障害）は Worker に写る（呼び出し側で 503 へ）。
#[tokio::test]
async fn worker_5xx_maps_to_worker_error() {
    let base = spawn_worker("boom", Arc::new(Mutex::new(Seen::default()))).await;
    let composer = DocxComposer::new(reqwest::Client::new(), &base);
    let err = composer.compose("default", "x.docx", "本文").await;
    assert!(matches!(err, Err(OfficeError::Worker(_))), "{err:?}");
}

/// applied_ops=0（本文未反映）は成功扱いにせず Worker エラーへ（黙って空文書を返さない）。
#[tokio::test]
async fn zero_applied_ops_is_worker_error() {
    let base = spawn_worker("noop", Arc::new(Mutex::new(Seen::default()))).await;
    let composer = DocxComposer::new(reqwest::Client::new(), &base);
    let err = composer
        .compose("default", "x.docx", "反映されない本文")
        .await;
    assert!(matches!(err, Err(OfficeError::Worker(_))), "{err:?}");
}
