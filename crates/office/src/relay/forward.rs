//! HTTP 要求の転送（中継 → Collabora）。

use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};

use super::RelayConfig;

/// 転送しないヘッダ（hop-by-hop）。
///
/// Host は**残す**。coolwsd は `server_name` 未設定時に Host から自分の URL を組み立て、
/// WS では Origin と Host の一致を見る（Collabora 公式の nginx 例も `Host $host`）。
const DROPPED_HEADERS: &[HeaderName] = &[
    header::CONNECTION,
    header::PROXY_AUTHENTICATE,
    header::PROXY_AUTHORIZATION,
    header::TE,
    header::TRAILER,
    header::TRANSFER_ENCODING,
    header::UPGRADE,
];

fn is_dropped(name: &HeaderName) -> bool {
    DROPPED_HEADERS.contains(name) || name.as_str().eq_ignore_ascii_case("keep-alive")
}

/// 上流の URL（ベース＋パス＋クエリ）。
pub(super) fn upstream_url(base: &str, path: &str, query: Option<&str>) -> String {
    let base = base.trim_end_matches('/');
    match query {
        Some(q) if !q.is_empty() => format!("{base}{path}?{q}"),
        _ => format!("{base}{path}"),
    }
}

fn copy_headers(from: &HeaderMap, to: &mut HeaderMap) {
    for (name, value) in from {
        if !is_dropped(name) {
            to.append(name.clone(), value.clone());
        }
    }
}

/// 1 要求を上流へ転送し、応答をそのまま（本文はストリームで）返す。
pub(super) async fn forward(
    http: &reqwest::Client,
    config: &RelayConfig,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();
    let url = upstream_url(&config.upstream_base, parts.uri.path(), parts.uri.query());
    let Ok(body) = axum::body::to_bytes(body, config.max_body_bytes).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let mut headers = HeaderMap::new();
    copy_headers(&parts.headers, &mut headers);
    let upstream = http
        .request(parts.method.clone(), url)
        .headers(headers)
        .body(body)
        .send()
        .await;
    let upstream = match upstream {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, path = %parts.uri.path(), "office relay: Collabora へ転送できません");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let mut response = Response::builder().status(upstream.status());
    if let Some(h) = response.headers_mut() {
        copy_headers(upstream.headers(), h);
    }
    response
        .body(Body::from_stream(upstream.bytes_stream()))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_upstream_url_with_and_without_query() {
        assert_eq!(
            upstream_url("http://collabora:9980/", "/hosting/discovery", None),
            "http://collabora:9980/hosting/discovery"
        );
        assert_eq!(
            upstream_url(
                "http://collabora:9980",
                "/cool/clipboard",
                Some("WOPISrc=x&ViewId=1")
            ),
            "http://collabora:9980/cool/clipboard?WOPISrc=x&ViewId=1"
        );
    }

    #[test]
    fn drops_hop_by_hop_headers() {
        let mut from = HeaderMap::new();
        from.insert(header::HOST, "relay:9980".parse().unwrap());
        from.insert(header::CONNECTION, "keep-alive".parse().unwrap());
        from.insert("keep-alive", "timeout=5".parse().unwrap());
        from.insert(header::CONTENT_TYPE, "text/html".parse().unwrap());
        let mut to = HeaderMap::new();
        copy_headers(&from, &mut to);
        assert_eq!(to.len(), 2);
        assert!(to.contains_key(header::CONTENT_TYPE));
        assert!(to.contains_key(header::HOST), "Host は上流へ渡す");
    }
}
