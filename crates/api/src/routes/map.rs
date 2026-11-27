//! 背景地図の配信（`/map/*`・design.md §4.7）。
//!
//! Yui の ```ui の `map` は地点の位置関係と道順だけを描き、背景の地図はホスト（web）が `renderMap`
//! で重ねる。背景の地図タイルはここで配る PMTiles（OpenMapTiles 形式・OSM 由来）から読み、
//! ブラウザは shiki 以外へ接続しない（グリフ・スプライトも外から読まない）。
//!
//! - `GET /map/config` — 地図データがあるか・スタイルの場所・出典
//! - `GET /map/style.json` — MapLibre のスタイル（ソースは `pmtiles://`）
//! - `GET /map/basemap.pmtiles` — 地図データ本体を範囲指定（`Range`）で配る
//!
//! 地図データはテナントに属さない共通の資材なので、テナントのデータには触れない。
//! それでもログイン中の利用者にだけ配る（Session）。

use std::{io::SeekFrom, path::Path};

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use utoipa::{IntoParams, ToSchema};

use super::map_style::{basemap_style, MapTheme, ATTRIBUTION};
use crate::{
    error::ApiError,
    extract::AuthContextExt,
    server::{AccessPolicy, RouteDecl},
    state::AppState,
};

/// スタイルの場所（API の根からの相対パス。web は自分の API の入口を前に付けて読む）。
pub const STYLE_PATH: &str = "/map/style.json";
/// 地図データの場所（API の根からの相対パス）。スタイルの中では `pmtiles://` を前に付けて書く。
pub const ARCHIVE_PATH: &str = "/map/basemap.pmtiles";
/// 1 回の範囲指定で返す上限（バイト）。PMTiles の読み手はヘッダ（16 KiB）・ディレクトリ・
/// タイルを個別に読むので、通常は数百 KiB に収まる。巨大な範囲で 1 リクエストを長く占有させない。
pub const MAX_RANGE_BYTES: u64 = 16 * 1024 * 1024;
/// 範囲を流すときの読み出しの単位（バイト）。
const READ_CHUNK_BYTES: usize = 64 * 1024;
/// 地図データの応答に付ける `Content-Type`。
const PMTILES_CONTENT_TYPE: &str = "application/vnd.pmtiles";

pub(crate) fn map_route_decls() -> Vec<RouteDecl> {
    vec![
        RouteDecl::new("/map/config", &["GET"], AccessPolicy::Session, || {
            get(map_config)
        }),
        RouteDecl::new(STYLE_PATH, &["GET"], AccessPolicy::Session, || {
            get(map_style)
        }),
        RouteDecl::new(ARCHIVE_PATH, &["GET"], AccessPolicy::Session, || {
            get(basemap_archive)
        }),
    ]
}

/// 出典の 1 項目（表示する文字と、権利表記のページ）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MapAttribution {
    pub text: String,
    pub url: String,
}

/// `GET /map/config` のレスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct MapConfigResponse {
    /// 地図データが配備されているか。`false` なら web は背景なしの図（Yui の静止画）を描く。
    pub available: bool,
    /// スタイル JSON の場所（API の根からの相対パス。`available` が `false` なら `null`）。
    /// `?theme=light|dark` で配色を選ぶ。
    pub style_url: Option<String>,
    /// 地図に必ず表示する出典（地図データの利用条件）。
    pub attribution: Vec<MapAttribution>,
}

/// 地図データの置き場所（設定とファイルの有無）。
async fn archive_path(state: &AppState) -> Option<std::path::PathBuf> {
    let path = state.config.map.basemap_path()?;
    match tokio::fs::metadata(&path).await {
        Ok(m) if m.is_file() => Some(path),
        _ => None,
    }
}

/// 地図データがあるか・スタイルの場所・出典を返す。
#[utoipa::path(
    get,
    path = "/map/config",
    responses(
        (status = 200, description = "地図の設定", body = MapConfigResponse),
        (status = 401, description = "未認証"),
    ),
    security(("session" = [])),
)]
pub async fn map_config(
    State(state): State<AppState>,
    AuthContextExt(_ctx): AuthContextExt,
) -> Json<MapConfigResponse> {
    let available = archive_path(&state).await.is_some();
    Json(MapConfigResponse {
        available,
        style_url: available.then(|| STYLE_PATH.to_string()),
        attribution: ATTRIBUTION
            .iter()
            .map(|(text, url)| MapAttribution {
                text: (*text).to_string(),
                url: (*url).to_string(),
            })
            .collect(),
    })
}

/// `GET /map/style.json` のクエリ。
#[derive(Debug, Deserialize, IntoParams)]
pub struct MapStyleQuery {
    /// 配色（`light` / `dark`・既定 `light`）。
    #[serde(default)]
    pub theme: Option<MapTheme>,
}

/// MapLibre のスタイル JSON。ソースは `pmtiles:///map/basemap.pmtiles`
/// （`pmtiles://` の後ろは API の根からの相対パス。web が自分の API の入口へ読み替える）。
/// `glyphs` と `sprite` は持たない（文字は端末のフォントで描き、外から何も読まない）。
#[utoipa::path(
    get,
    path = "/map/style.json",
    params(MapStyleQuery),
    responses(
        (status = 200, description = "MapLibre のスタイル JSON", body = Object),
        (status = 401, description = "未認証"),
        (status = 404, description = "地図データが配備されていない"),
    ),
    security(("session" = [])),
)]
pub async fn map_style(
    State(state): State<AppState>,
    AuthContextExt(_ctx): AuthContextExt,
    Query(q): Query<MapStyleQuery>,
) -> Result<Response, ApiError> {
    if archive_path(&state).await.is_none() {
        return Err(ApiError::NotFound);
    }
    let style = basemap_style(q.theme.unwrap_or_default(), ARCHIVE_PATH);
    let mut res = Json(style).into_response();
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=300"),
    );
    Ok(res)
}

/// 範囲指定の解釈結果。
#[derive(Debug, PartialEq, Eq)]
enum RangeSpec {
    /// 返す範囲（両端を含む）。
    Satisfiable { start: u64, end: u64 },
    /// 範囲がファイルの外・形式が不正・複数範囲・上限超え（416）。
    Unsatisfiable,
}

/// `Range: bytes=…` を 1 つの範囲として解釈する（RFC 9110 §14.1.2）。
/// 複数範囲（`,`）は扱わない（PMTiles の読み手は 1 範囲ずつ読む）。
fn parse_range(value: &str, len: u64) -> RangeSpec {
    let Some(spec) = value.trim().strip_prefix("bytes=") else {
        return RangeSpec::Unsatisfiable;
    };
    if spec.contains(',') || len == 0 {
        return RangeSpec::Unsatisfiable;
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return RangeSpec::Unsatisfiable;
    };
    let (first, last) = (first.trim(), last.trim());
    let parse = |s: &str| -> Option<u64> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    };
    let (start, end) = if first.is_empty() {
        // 末尾からの長さ（`bytes=-N`）。
        let Some(n) = parse(last).filter(|n| *n > 0) else {
            return RangeSpec::Unsatisfiable;
        };
        (len.saturating_sub(n), len - 1)
    } else {
        let Some(start) = parse(first) else {
            return RangeSpec::Unsatisfiable;
        };
        let end = if last.is_empty() {
            len - 1
        } else {
            match parse(last) {
                Some(e) if e >= start => e.min(len - 1),
                _ => return RangeSpec::Unsatisfiable,
            }
        };
        (start, end)
    };
    if start >= len || end - start + 1 > MAX_RANGE_BYTES {
        return RangeSpec::Unsatisfiable;
    }
    RangeSpec::Satisfiable { start, end }
}

/// ファイルの版を表す強い ETag（大きさと更新時刻）。差し替えると変わるので、PMTiles の読み手
/// （pmtiles.js）は手元の ETag と比べて違えばヘッダから読み直す。`If-Match` を付けて読む
/// 読み手には、版が外れたら 412 を返す。
fn archive_etag(meta: &std::fs::Metadata) -> String {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    format!("\"{:x}-{:x}\"", meta.len(), mtime)
}

/// `If-Match` が今の版と合うか（無ければ合うとみなす）。
fn if_match_ok(headers: &HeaderMap, etag: &str) -> bool {
    let Some(v) = headers.get(header::IF_MATCH) else {
        return true;
    };
    let Ok(v) = v.to_str() else {
        return false;
    };
    v.split(',').any(|t| {
        let t = t.trim();
        t == "*" || t == etag
    })
}

fn header_value(s: &str) -> Result<HeaderValue, ApiError> {
    HeaderValue::from_str(s).map_err(|e| ApiError::Internal(format!("map header: {e}")))
}

/// 地図データ（PMTiles）を範囲指定で配る。
///
/// 範囲指定の無い `GET` は受け付けない（416）。数百 MB〜数 GB のファイルを 1 リクエストで
/// 流させない。`HEAD` は大きさと版だけを返す。
#[utoipa::path(
    get,
    path = "/map/basemap.pmtiles",
    params(
        ("Range" = String, Header, description = "読む範囲（`bytes=start-end`・1 範囲だけ）"),
    ),
    responses(
        (status = 206, description = "指定した範囲", content_type = "application/vnd.pmtiles"),
        (status = 401, description = "未認証"),
        (status = 404, description = "地図データが配備されていない"),
        (status = 412, description = "If-Match の版が今の地図データと違う（読み直す）"),
        (status = 416, description = "範囲指定が無い・不正・ファイルの外・上限超え"),
    ),
    security(("session" = [])),
)]
pub async fn basemap_archive(
    State(state): State<AppState>,
    AuthContextExt(_ctx): AuthContextExt,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let path = archive_path(&state).await.ok_or(ApiError::NotFound)?;
    serve_range(&path, &method, &headers).await
}

async fn serve_range(
    path: &Path,
    method: &Method,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    // 開いたファイルの情報を使う（確かめてから開くまでの間に差し替わっても、版と中身を揃える）。
    let mut file = tokio::fs::File::open(path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ApiError::NotFound
        } else {
            ApiError::Internal(format!("basemap open: {e}"))
        }
    })?;
    let meta = file
        .metadata()
        .await
        .map_err(|e| ApiError::Internal(format!("basemap metadata: {e}")))?;
    let len = meta.len();
    let etag = archive_etag(&meta);

    let mut builder = Response::builder()
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, header_value(&etag)?)
        .header(header::CACHE_CONTROL, "private, max-age=300")
        .header(header::CONTENT_TYPE, PMTILES_CONTENT_TYPE);

    if !if_match_ok(headers, &etag) {
        return builder
            .status(StatusCode::PRECONDITION_FAILED)
            .body(Body::empty())
            .map_err(|e| ApiError::Internal(format!("basemap response: {e}")));
    }
    if method == Method::HEAD && !headers.contains_key(header::RANGE) {
        return builder
            .status(StatusCode::OK)
            .header(header::CONTENT_LENGTH, len)
            .body(Body::empty())
            .map_err(|e| ApiError::Internal(format!("basemap response: {e}")));
    }
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map_or(RangeSpec::Unsatisfiable, |v| parse_range(v, len));
    let RangeSpec::Satisfiable { start, end } = range else {
        return builder
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(
                header::CONTENT_RANGE,
                header_value(&format!("bytes */{len}"))?,
            )
            .body(Body::empty())
            .map_err(|e| ApiError::Internal(format!("basemap response: {e}")));
    };
    let count = end - start + 1;
    builder = builder
        .status(StatusCode::PARTIAL_CONTENT)
        .header(
            header::CONTENT_RANGE,
            header_value(&format!("bytes {start}-{end}/{len}"))?,
        )
        .header(header::CONTENT_LENGTH, count);
    if method == Method::HEAD {
        return builder
            .body(Body::empty())
            .map_err(|e| ApiError::Internal(format!("basemap response: {e}")));
    }
    file.seek(SeekFrom::Start(start))
        .await
        .map_err(|e| ApiError::Internal(format!("basemap seek: {e}")))?;
    // 範囲を丸ごとメモリに載せず、小分けに読みながら流す（同時に多数の範囲を読まれても、
    // 1 リクエストあたりのメモリは読み出しの単位までに収まる）。
    let stream = futures::stream::try_unfold(file.take(count), |mut r| async move {
        let mut chunk = vec![0u8; READ_CHUNK_BYTES];
        let n = r.read(&mut chunk).await?;
        if n == 0 {
            return Ok::<_, std::io::Error>(None);
        }
        chunk.truncate(n);
        Ok(Some((axum::body::Bytes::from(chunk), r)))
    });
    builder
        .body(Body::from_stream(stream))
        .map_err(|e| ApiError::Internal(format!("basemap response: {e}")))
}

#[cfg(test)]
#[path = "map_tests.rs"]
mod tests;
