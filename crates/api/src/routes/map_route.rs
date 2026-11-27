//! `POST /map/route` — 道路沿いの道順（Yui の `route:` を地図に重ねる線・design.md §4.7）。
//!
//! 自前で動かす Valhalla（`SHIKI__MAP__ROUTING_URL`）に経路を問い合わせ、地図に描く線（経度・緯度の列）と
//! 距離・所要時間を返す。Valhalla を呼ぶのは shiki-server だけで、ブラウザは shiki 以外へ接続しない。
//! 公共交通（`transit`）は時刻表データが無いため扱わない（web は直線のまま描く）。
//!
//! 地点はリクエストの本文から受け取る（Yui の図に書かれた緯度経度）。テナントのデータには触れない。

use std::{sync::OnceLock, time::Duration};

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::ToSchema;

use tokio::sync::Semaphore;

use crate::{error::ApiError, extract::AuthContextExt, state::AppState};

/// 1 回に渡せる地点の数（Yui の `map` の上限と同じ）。
pub const MAX_STOPS: usize = 50;
/// Valhalla の応答を待つ上限。
const ROUTING_TIMEOUT: Duration = Duration::from_secs(15);
/// Valhalla の応答本文の上限（バイト）。経路の形は 50 地点でも数百 KB に収まる。
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// Valhalla へ同時に送る問い合わせの上限（プロセス全体）。重い経路計算を並べて送られても、
/// 計算先を詰まらせてほかの利用者の道順を止めない。
const MAX_CONCURRENT: usize = 4;
/// 空きを待つ上限。待っても空かなければ 503（web は直線のまま描く）。
const QUEUE_WAIT: Duration = Duration::from_secs(3);

fn routing_slots() -> &'static Semaphore {
    static SLOTS: OnceLock<Semaphore> = OnceLock::new();
    SLOTS.get_or_init(|| Semaphore::new(MAX_CONCURRENT))
}

/// 移動手段（Yui の `route:` の `walk` / `drive`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RouteMode {
    Walk,
    Drive,
}

impl RouteMode {
    /// Valhalla の costing。
    fn costing(self) -> &'static str {
        match self {
            RouteMode::Walk => "pedestrian",
            RouteMode::Drive => "auto",
        }
    }
}

/// 経由地（10 進度）。
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
pub struct RouteStop {
    pub lat: f64,
    pub lng: f64,
}

/// `POST /map/route` の本文。
#[derive(Debug, Deserialize, ToSchema)]
pub struct RouteRequest {
    pub mode: RouteMode,
    /// 通る順の地点（2〜50）。
    pub stops: Vec<RouteStop>,
}

/// `POST /map/route` の応答。
#[derive(Debug, Serialize, ToSchema)]
pub struct RouteResponse {
    /// 道順の線（`[経度, 緯度]` の列・GeoJSON の LineString の座標と同じ並び）。
    pub coordinates: Vec<[f64; 2]>,
    /// 全体の距離（メートル）。
    pub distance_m: f64,
    /// 全体の所要時間の目安（秒）。
    pub duration_s: f64,
}

fn validate(req: &RouteRequest) -> Result<(), ApiError> {
    if req.stops.len() < 2 || req.stops.len() > MAX_STOPS {
        return Err(ApiError::BadRequest(format!(
            "地点は 2〜{MAX_STOPS} 個で指定してください"
        )));
    }
    let ok = req.stops.iter().all(|s| {
        s.lat.is_finite()
            && s.lng.is_finite()
            && (-90.0..=90.0).contains(&s.lat)
            && (-180.0..=180.0).contains(&s.lng)
    });
    if !ok {
        return Err(ApiError::BadRequest("緯度・経度が範囲の外です".into()));
    }
    Ok(())
}

/// Valhalla の `shape`（精度 6 桁の encoded polyline）を `[経度, 緯度]` の列に戻す。
fn decode_polyline6(s: &str) -> Option<Vec<[f64; 2]>> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let (mut lat, mut lng) = (0i64, 0i64);
    let mut out = Vec::new();
    let next = |i: &mut usize| -> Option<i64> {
        let (mut result, mut shift) = (0i64, 0u32);
        loop {
            let b = i64::from(*bytes.get(*i)?) - 63;
            *i += 1;
            if !(0..64).contains(&b) || shift > 60 {
                return None;
            }
            result |= (b & 0x1f) << shift;
            shift += 5;
            if b < 0x20 {
                break;
            }
        }
        Some(if result & 1 == 1 {
            !(result >> 1)
        } else {
            result >> 1
        })
    };
    while i < bytes.len() {
        lat = lat.checked_add(next(&mut i)?)?;
        lng = lng.checked_add(next(&mut i)?)?;
        // 地球の上に無い点は壊れた形として扱う（範囲外の座標を描かせない）。
        if lat.abs() > 90_000_000 || lng.abs() > 180_000_000 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)] // 1e-6 度単位で範囲内の整数。f64 の仮数に収まる。
        out.push([lng as f64 / 1e6, lat as f64 / 1e6]);
    }
    Some(out)
}

/// Valhalla の応答から線・距離・時間を取り出す（区間ごとの線をつなぐ）。
fn parse_trip(body: &Value) -> Option<RouteResponse> {
    let trip = body.get("trip")?;
    let mut coordinates: Vec<[f64; 2]> = Vec::new();
    for leg in trip.get("legs")?.as_array()? {
        let shape = decode_polyline6(leg.get("shape")?.as_str()?)?;
        // 区間の始点は前の区間の終点と同じなので重ねない。
        let skip = usize::from(!coordinates.is_empty() && !shape.is_empty());
        coordinates.extend(shape.into_iter().skip(skip));
    }
    let summary = trip.get("summary")?;
    // Valhalla は units を指定しなければキロメートルで返す。
    let distance_m = summary.get("length")?.as_f64()? * 1000.0;
    let duration_s = summary.get("time")?.as_f64()?;
    (coordinates.len() >= 2).then_some(RouteResponse {
        coordinates,
        distance_m,
        duration_s,
    })
}

/// 道路沿いの道順を返す。
#[utoipa::path(
    post,
    path = "/map/route",
    request_body = RouteRequest,
    responses(
        (status = 200, description = "道順の線と距離・時間", body = RouteResponse),
        (status = 400, description = "地点の数・緯度経度が不正"),
        (status = 401, description = "未認証"),
        (status = 404, description = "道順の計算先が設定されていない"),
        (status = 422, description = "道路で結べない（地点が経路データの範囲外など）。`code` は `no_route`"),
        (status = 503, description = "道順の計算先に届かない"),
    ),
    security(("session" = [])),
)]
pub async fn map_route(
    State(state): State<AppState>,
    AuthContextExt(_ctx): AuthContextExt,
    Json(req): Json<RouteRequest>,
) -> Result<Json<RouteResponse>, ApiError> {
    let base = state.config.map.routing_base().ok_or(ApiError::NotFound)?;
    validate(&req)?;
    let body = json!({
        "locations": req.stops.iter().map(|s| json!({ "lat": s.lat, "lon": s.lng })).collect::<Vec<_>>(),
        "costing": req.mode.costing(),
        "directions_type": "none",
    });
    let _slot = tokio::time::timeout(QUEUE_WAIT, routing_slots().acquire())
        .await
        .map_err(|_| ApiError::ServiceUnavailable("routing: busy".into()))?
        .map_err(|e| ApiError::ServiceUnavailable(format!("routing slots: {e}")))?;
    let mut res = state
        .http
        .post(format!("{base}/route"))
        .timeout(ROUTING_TIMEOUT)
        .json(&body)
        .send()
        .await
        .map_err(|e| ApiError::ServiceUnavailable(format!("routing: {e}")))?;
    let status = res.status();
    // 受け取りながら上限を見る（巨大な本文を全部メモリに載せてから捨てない）。
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = res
        .chunk()
        .await
        .map_err(|e| ApiError::ServiceUnavailable(format!("routing body: {e}")))?
    {
        if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(ApiError::ServiceUnavailable(
                "routing: response too large".into(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    if status.is_client_error() {
        // Valhalla は地点が道路から遠い・結べない・地点数の上限を超えたときに、`error_code` を付けた 400 を返す。
        // 図の地点が原因なので 422 にする。それ以外の 4xx（404・405 など）は計算先の取り違えなので 503。
        let valhalla_error = status == reqwest::StatusCode::BAD_REQUEST
            && serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|v| v.get("error_code").and_then(Value::as_i64))
                .is_some();
        if valhalla_error {
            tracing::info!(status = %status, "道順を計算できない地点");
            return Err(ApiError::Unprocessable422(json!({ "code": "no_route" })));
        }
        return Err(ApiError::ServiceUnavailable(format!(
            "routing status {status}（SHIKI__MAP__ROUTING_URL が Valhalla を指しているか確認）"
        )));
    }
    if !status.is_success() {
        return Err(ApiError::ServiceUnavailable(format!(
            "routing status {status}"
        )));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|e| ApiError::ServiceUnavailable(format!("routing json: {e}")))?;
    parse_trip(&value)
        .map(Json)
        .ok_or_else(|| ApiError::ServiceUnavailable("routing: unexpected response".into()))
}

#[cfg(test)]
#[path = "map_route_tests.rs"]
mod tests;
