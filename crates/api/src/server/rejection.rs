//! 要求の形の誤りの応答をサーバ全体で構造化する（検証 N14・N10・単独サロゲートの 400）。
//!
//! axum の抽出器（`Json` / `Path` / `Query`）の拒否とルータの 404 / 405 は、英語の平文（`missing field
//! \`expected_rev\``・`Failed to deserialize …`）か本文なしで返る。ハンドラごとに抽出器を差し替えると漏れが
//! 出るので、**応答の側で 1 箇所**に寄せる: JSON でない 4xx（平文・本文なし）を
//! [`crate::error_body::RequestErrorBody`]（400 は `BadRequestBody` と同じ形・`code: invalid_input`）へ
//! 置き換える。文言は利用者向けの日本語で、内部の型名・ライブラリの英語の文言を含めない（serde の
//! 文言から項目名・位置だけを拾う）。ハンドラが返す構造化本文（JSON）はそのまま通す。

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

/// 置き換えのために読む平文の本文の上限（axum の拒否の文言は短い）。
const MAX_PLAIN_BODY: usize = 16 * 1024;

/// JSON でない 4xx を構造化本文へ置き換える（`axum::middleware::map_response` 用）。
pub(crate) async fn structure_plain_errors(resp: Response) -> Response {
    let status = resp.status();
    if !status.is_client_error() {
        return resp;
    }
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let plain = ct.is_empty() || ct.starts_with("text/plain");
    if !plain {
        return resp;
    }
    let (parts, body) = resp.into_parts();
    let text = match axum::body::to_bytes(body, MAX_PLAIN_BODY).await {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(_) => String::new(),
    };
    let (status, code, message) = classify(status, &text);
    if !text.is_empty() {
        tracing::info!(status = status.as_u16(), reason = %text, "要求の形の誤り（構造化して返す）");
    }
    let body = crate::error_body::RequestErrorBody {
        status: status.as_u16(),
        title: status.canonical_reason().unwrap_or("Error").into(),
        code: code.into(),
        message,
    };
    let mut out = (status, axum::Json(body)).into_response();
    // 元の応答のヘッダ（`Allow`・`Retry-After` など）は残す（本文の種類と長さは新しい本文のもの）。
    for (k, v) in &parts.headers {
        if k != header::CONTENT_TYPE && k != header::CONTENT_LENGTH {
            out.headers_mut().insert(k.clone(), v.clone());
        }
    }
    out.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    out
}

/// 拒否の種類ごとの status・code・文言。
fn classify(status: StatusCode, text: &str) -> (StatusCode, &'static str, String) {
    const JSON_DATA: &str = "Failed to deserialize the JSON body into the target type: ";
    const JSON_SYNTAX: &str = "Failed to parse the request body as JSON: ";
    const QUERY: &str = "Failed to deserialize query string: ";
    const PATH: &str = "Invalid URL: ";
    if let Some(rest) = text
        .strip_prefix(JSON_DATA)
        .or_else(|| text.strip_prefix(JSON_SYNTAX))
    {
        return (
            StatusCode::BAD_REQUEST,
            "invalid_input",
            format!("本文（JSON）を読めません: {}", serde_message(rest)),
        );
    }
    if let Some(rest) = text.strip_prefix(QUERY) {
        return (
            StatusCode::BAD_REQUEST,
            "invalid_input",
            format!("クエリ文字列を読めません: {}", serde_message(rest)),
        );
    }
    if text.starts_with(PATH) || text.starts_with("Wrong number of path arguments") {
        return (
            StatusCode::BAD_REQUEST,
            "invalid_input",
            "URL の値が正しくありません（識別子は UUID で指定してください）".into(),
        );
    }
    match status {
        StatusCode::UNSUPPORTED_MEDIA_TYPE => (
            status,
            "unsupported_media_type",
            "本文の種類が正しくありません（Content-Type: application/json で送ってください）"
                .into(),
        ),
        StatusCode::PAYLOAD_TOO_LARGE => (status, "payload_too_large", "本文が大きすぎます".into()),
        StatusCode::NOT_FOUND => (status, "not_found", "見つかりません".into()),
        StatusCode::METHOD_NOT_ALLOWED => (
            status,
            "method_not_allowed",
            "この操作（HTTP メソッド）は使えません".into(),
        ),
        StatusCode::UNAUTHORIZED => (status, "unauthorized", "ログインが必要です".into()),
        StatusCode::FORBIDDEN => (status, "forbidden", "権限がありません".into()),
        StatusCode::BAD_REQUEST => (status, "invalid_input", "要求の形が正しくありません".into()),
        _ => (status, "bad_request", "要求を処理できません".into()),
    }
}

/// serde の文言を利用者向けの日本語へ（項目名と位置だけを拾い、型名・英語の文言を出さない）。
pub(crate) fn serde_message(raw: &str) -> String {
    let (msg, pos) = split_position(raw);
    let quoted = |s: &str| -> Option<String> {
        let start = s.find('`')? + 1;
        let end = start + s[start..].find('`')?;
        Some(s[start..end].to_string())
    };
    let what = if let Some(rest) = msg.strip_prefix("missing field ") {
        quoted(rest).map_or_else(
            || "必要な項目がありません".to_string(),
            |f| format!("項目 '{f}' がありません"),
        )
    } else if let Some(rest) = msg.strip_prefix("unknown field ") {
        quoted(rest).map_or_else(
            || "使えない項目があります".to_string(),
            |f| format!("項目 '{f}' は使えません"),
        )
    } else if msg.starts_with("duplicate field ") {
        quoted(msg).map_or_else(
            || "項目が重複しています".to_string(),
            |f| format!("項目 '{f}' が重複しています"),
        )
    } else if msg.starts_with("unknown variant ") {
        "選択肢にない値があります".to_string()
    } else if msg.starts_with("invalid type") {
        "値の型が正しくありません".to_string()
    } else if msg.starts_with("invalid value") || msg.starts_with("invalid digit") {
        "値が正しくありません".to_string()
    } else if msg.starts_with("invalid length") {
        "値の数が正しくありません".to_string()
    } else if msg.contains("surrogate")
        || msg.contains("unicode code point")
        || msg.contains("escape")
    {
        "文字列に使えない文字（単独のサロゲートなど）があります".to_string()
    } else if msg.starts_with("EOF while parsing") {
        "JSON が途中で終わっています".to_string()
    } else if msg.contains("UUID") || msg.contains("uuid") {
        "識別子（UUID）の形が正しくありません".to_string()
    } else if msg.contains("number") || msg.contains("integer") {
        "数値の形が正しくありません".to_string()
    } else {
        "JSON の形が正しくありません".to_string()
    };
    match pos {
        Some((line, col)) => format!("{what}（{line} 行 {col} 列目）"),
        None => what,
    }
}

/// serde_json の「… at line L column C」の位置を外す。
fn split_position(raw: &str) -> (&str, Option<(u64, u64)>) {
    if let Some(i) = raw.rfind(" at line ") {
        let tail = &raw[i + " at line ".len()..];
        let mut it = tail.split(" column ");
        if let (Some(l), Some(c)) = (it.next(), it.next()) {
            if let (Ok(l), Ok(c)) = (l.trim().parse(), c.trim().parse()) {
                return (&raw[..i], Some((l, c)));
            }
        }
    }
    (raw, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn japanese_only(s: &str) {
        for bad in [
            "expected",
            "struct",
            "Failed",
            "missing field",
            "deserialize",
            "at line",
        ] {
            assert!(!s.contains(bad), "{s}");
        }
    }

    #[test]
    fn serde_messages_are_translated_without_type_names() {
        let m = serde_message("missing field `expected_rev` at line 1 column 2");
        assert_eq!(m, "項目 'expected_rev' がありません（1 行 2 列目）");
        let m = serde_message(
            "invalid type: string \"x\", expected struct StartImportRequest at line 1 column 3",
        );
        japanese_only(&m);
        assert!(m.contains("型"));
        let m = serde_message("lone leading surrogate in hex escape at line 1 column 15");
        japanese_only(&m);
        assert!(m.contains("サロゲート"));
        let m = serde_message("unknown variant `zz`, expected one of `a`, `b`");
        japanese_only(&m);
    }

    #[test]
    fn rejections_are_classified() {
        let (s, c, m) = classify(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Failed to deserialize the JSON body into the target type: missing field `data` at line 1 column 2",
        );
        assert_eq!((s, c), (StatusCode::BAD_REQUEST, "invalid_input"));
        assert!(m.contains("'data'"));
        japanese_only(&m);
        let (s, c, m) = classify(
            StatusCode::BAD_REQUEST,
            "Invalid URL: UUID parsing failed: invalid character",
        );
        assert_eq!((s, c), (StatusCode::BAD_REQUEST, "invalid_input"));
        assert!(m.contains("UUID"));
        let (s, c, _) = classify(
            StatusCode::BAD_REQUEST,
            "Failed to deserialize query string: missing field `expected_rev`",
        );
        assert_eq!((s, c), (StatusCode::BAD_REQUEST, "invalid_input"));
        let (s, c, _) = classify(StatusCode::METHOD_NOT_ALLOWED, "");
        assert_eq!(
            (s, c),
            (StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed")
        );
    }

    #[tokio::test]
    async fn plain_rejections_become_json_and_json_passes_through() {
        let plain = (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Failed to deserialize the JSON body into the target type: missing field `expected_rev` at line 1 column 2",
        )
            .into_response();
        let out = structure_plain_errors(plain).await;
        assert_eq!(out.status(), StatusCode::BAD_REQUEST);
        let b = axum::body::to_bytes(out.into_body(), 1 << 16)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(v["code"], "invalid_input");
        assert_eq!(v["status"], 400);
        let json = (
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({"code": "x"})),
        )
            .into_response();
        let out = structure_plain_errors(json).await;
        let b = axum::body::to_bytes(out.into_body(), 1 << 16)
            .await
            .unwrap();
        assert_eq!(b.as_ref(), br#"{"code":"x"}"#);
        // 本文なしの 400 も構造化する。
        let empty = StatusCode::BAD_REQUEST.into_response();
        let out = structure_plain_errors(empty).await;
        let b = axum::body::to_bytes(out.into_body(), 1 << 16)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(v["code"], "invalid_input");
    }
}
