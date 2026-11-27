//! 背景地図の配信（`/map/*`）の HTTP テスト。地図データは中身を決めたダミーのファイルで代用する
//! （配信は範囲を切り出すだけで、PMTiles の中身は読まない）。

use super::*;
use axum::http::header::{ACCEPT_RANGES, CONTENT_RANGE, ETAG, IF_MATCH, RANGE};

/// 0..len のバイト列を書いた地図データ置き場（テストごとに別のディレクトリ）。
fn basemap_dir(len: usize) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("shiki-map-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let bytes: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
    std::fs::write(dir.join("basemap.pmtiles"), bytes).unwrap();
    dir
}

async fn app_with_map(data_dir: Option<&std::path::Path>) -> axum::Router {
    let mut state = state_with(store_with_valid_session().await, None);
    let mut config = (*state.config).clone();
    config.map.data_dir = data_dir.map(|d| d.display().to_string());
    state.config = Arc::new(config);
    build_router(state)
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    headers: &[(axum::http::HeaderName, &str)],
) -> axum::http::Response<Body> {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(COOKIE, "shiki_session=sid-valid.default");
    for (k, v) in headers {
        req = req.header(k, *v);
    }
    app.clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn body_bytes(resp: axum::http::Response<Body>) -> Vec<u8> {
    resp.into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

#[tokio::test]
async fn map_routes_require_a_session() {
    let dir = basemap_dir(64);
    let app = app_with_map(Some(&dir)).await;
    for uri in ["/map/config", "/map/style.json", "/map/basemap.pmtiles"] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
}

#[tokio::test]
async fn without_basemap_config_reports_unavailable_and_hides_style() {
    // 置き場所が未設定。
    let app = app_with_map(None).await;
    let (status, body) = get_with_session(app.clone(), "/map/config").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["available"], false);
    assert!(body["style_url"].is_null());
    // 出典は地図データの有無にかかわらず返す。
    assert_eq!(
        body["attribution"][0]["text"],
        "© OpenStreetMap contributors"
    );
    assert_eq!(body["attribution"][1]["text"], "© OpenMapTiles");
    let resp = send(&app, "GET", "/map/style.json", &[]).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let resp = send(&app, "GET", "/map/basemap.pmtiles", &[(RANGE, "bytes=0-3")]).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // 置き場所はあるがファイルが無い。
    let empty = std::env::temp_dir().join(format!("shiki-map-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&empty).unwrap();
    let app = app_with_map(Some(&empty)).await;
    let (_, body) = get_with_session(app, "/map/config").await;
    assert_eq!(body["available"], false);
}

#[tokio::test]
async fn config_and_style_point_only_at_the_own_archive() {
    let dir = basemap_dir(64);
    let app = app_with_map(Some(&dir)).await;
    let (status, body) = get_with_session(app.clone(), "/map/config").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["available"], true);
    assert_eq!(body["style_url"], "/map/style.json");

    for theme in ["", "?theme=light", "?theme=dark"] {
        let (status, style) =
            get_with_session(app.clone(), &format!("/map/style.json{theme}")).await;
        assert_eq!(status, StatusCode::OK, "{theme}");
        assert_eq!(style["version"], 8);
        assert_eq!(
            style["sources"]["openmaptiles"]["url"],
            "pmtiles:///map/basemap.pmtiles"
        );
        // 外から読むもの（グリフ・スプライト）を持たない。
        assert!(style.get("glyphs").is_none());
        assert!(style.get("sprite").is_none());
        // 全レイヤが自分のソースだけを使い、URL を含まない。
        let text = style["layers"].to_string();
        assert!(!text.contains("http"), "{theme}");
        for layer in style["layers"].as_array().unwrap() {
            if let Some(src) = layer.get("source") {
                assert_eq!(src, "openmaptiles");
            }
        }
    }
    let resp = send(&app, "GET", "/map/style.json?theme=sepia", &[]).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn archive_serves_single_ranges_only() {
    let dir = basemap_dir(1000);
    let app = app_with_map(Some(&dir)).await;
    let expected: Vec<u8> = (0..1000usize).map(|i| (i % 251) as u8).collect();

    let resp = send(
        &app,
        "GET",
        "/map/basemap.pmtiles",
        &[(RANGE, "bytes=10-19")],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(resp.headers()[CONTENT_RANGE], "bytes 10-19/1000");
    assert_eq!(resp.headers()[ACCEPT_RANGES], "bytes");
    assert_eq!(
        resp.headers()[axum::http::header::CONTENT_TYPE],
        "application/vnd.pmtiles"
    );
    let etag = resp.headers()[ETAG].to_str().unwrap().to_string();
    assert!(etag.starts_with('"') && !etag.starts_with("W/"));
    assert_eq!(body_bytes(resp).await, expected[10..20]);

    // 末尾からの長さ・終わりを省いた範囲・ファイルを越える終わり。
    let resp = send(&app, "GET", "/map/basemap.pmtiles", &[(RANGE, "bytes=-5")]).await;
    assert_eq!(resp.headers()[CONTENT_RANGE], "bytes 995-999/1000");
    assert_eq!(body_bytes(resp).await, expected[995..]);
    let resp = send(
        &app,
        "GET",
        "/map/basemap.pmtiles",
        &[(RANGE, "bytes=990-")],
    )
    .await;
    assert_eq!(body_bytes(resp).await, expected[990..]);
    let resp = send(
        &app,
        "GET",
        "/map/basemap.pmtiles",
        &[(RANGE, "bytes=998-5000")],
    )
    .await;
    assert_eq!(resp.headers()[CONTENT_RANGE], "bytes 998-999/1000");

    // 範囲指定が無い・ファイルの外・複数範囲は 416（全体を 1 リクエストで流さない）。
    for range in [
        None,
        Some("bytes=1000-1001"),
        Some("bytes=0-1,5-6"),
        Some("items=0-1"),
    ] {
        let headers: Vec<_> = range.into_iter().map(|r| (RANGE, r)).collect();
        let resp = send(&app, "GET", "/map/basemap.pmtiles", &headers).await;
        assert_eq!(
            resp.status(),
            StatusCode::RANGE_NOT_SATISFIABLE,
            "{range:?}"
        );
        assert_eq!(resp.headers()[CONTENT_RANGE], "bytes */1000");
    }

    // 版が合えば読め、外れたら 412（読み手はヘッダから読み直す）。
    let resp = send(
        &app,
        "GET",
        "/map/basemap.pmtiles",
        &[(RANGE, "bytes=0-1"), (IF_MATCH, &etag)],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    let resp = send(
        &app,
        "GET",
        "/map/basemap.pmtiles",
        &[(RANGE, "bytes=0-1"), (IF_MATCH, "\"other\"")],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::PRECONDITION_FAILED);

    // HEAD は大きさと版だけ。
    let resp = send(&app, "HEAD", "/map/basemap.pmtiles", &[]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[axum::http::header::CONTENT_LENGTH], "1000");
    assert_eq!(resp.headers()[ETAG].to_str().unwrap(), etag);
    assert!(body_bytes(resp).await.is_empty());
    let resp = send(
        &app,
        "HEAD",
        "/map/basemap.pmtiles",
        &[(RANGE, "bytes=0-9")],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(resp.headers()[axum::http::header::CONTENT_LENGTH], "10");
    assert!(body_bytes(resp).await.is_empty());

    // 差し替えると版（ETag）が変わる（読み手は手元の版と比べて読み直す）。
    std::fs::write(dir.join("basemap.pmtiles"), vec![7u8; 1200]).unwrap();
    let resp = send(&app, "HEAD", "/map/basemap.pmtiles", &[]).await;
    assert_eq!(resp.headers()[axum::http::header::CONTENT_LENGTH], "1200");
    assert_ne!(resp.headers()[ETAG].to_str().unwrap(), etag);
}
