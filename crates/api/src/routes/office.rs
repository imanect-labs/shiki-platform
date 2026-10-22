//! Office 編集セッション発行（Task 11.6・design §4.8）。
//!
//! `/office/sessions` はブラウザ（cookie セッション）から呼ばれ、Collabora の編集
//! アクション URL と WOPI access_token を発行する。トークンは UX 用の入場券であり
//! 権限の根拠ではない（WOPI 側が毎呼び出しで ReBAC 再チェックする・PIT-11）。

use axum::extract::State;
use axum::routing::post;
use axum::Json;
use serde::{Deserialize, Serialize};
use storage::NodeKind;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthContextExt, TraceIdExt};
use crate::server::RouteDecl;
use crate::state::AppState;

/// office のルート宣言（route_table から分離・同じ宣言的マップの一部）。
///
/// `office.enabled=false` の構成では `build_router` がこの宣言を配線しない
/// （`/office/` プレフィックスで判定・fail-closed）。
pub(crate) fn office_route_decls() -> Vec<RouteDecl> {
    use crate::server::AccessPolicy::Session;
    let r = RouteDecl::new;
    vec![
        r("/office/sessions", &["POST"], Session, || {
            post(create_office_session)
        }),
        r("/office/charts", &["POST"], Session, || {
            post(insert_office_chart)
        }),
    ]
}

/// Office 編集セッションの発行リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateOfficeSessionRequest {
    /// 編集対象のファイル（docx/xlsx/pptx/odt/ods/odp）。
    pub file_id: Uuid,
}

/// Office 編集セッション（Collabora iframe の組み立て材料）。
#[derive(Debug, Serialize, ToSchema)]
pub struct OfficeSessionResponse {
    /// Collabora の編集アクション URL（discovery 由来・WOPISrc を付与して使う）。
    pub action_url: String,
    /// WOPISrc（Collabora から見た WOPI ファイル URL）。web は
    /// `action_url + "WOPISrc=" + encodeURIComponent(wopi_src)` に使う。
    pub wopi_src: String,
    /// WOPI access_token（実行主体×ファイル×短寿命。form post で iframe へ注入する）。
    pub access_token: String,
    /// トークンの有効期間（ミリ秒）。
    pub access_token_ttl_ms: u64,
}

/// Office 編集セッションを発行する。
///
/// viewer 権限が要る（無ければ存在秘匿の 404）。未対応拡張子・Collabora 未設定も
/// 404。トークンにはテナント境界と file_id を焼き込み、他ファイルへ流用できない。
#[utoipa::path(
    post, path = "/office/sessions", request_body = CreateOfficeSessionRequest,
    responses(
        (status = 200, description = "編集セッション", body = OfficeSessionResponse),
        (status = 401, description = "未認証"),
        (status = 404, description = "ファイルが存在しない・読めない（存在秘匿）・未対応拡張子"),
        (status = 503, description = "Collabora discovery の取得失敗（一時障害）"),
    ),
    security(("session" = [])),
)]
pub async fn create_office_session(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<CreateOfficeSessionRequest>,
) -> Result<Json<OfficeSessionResponse>, ApiError> {
    // enabled=false ではルート自体が配線されないが、防御的に 404 とする（fail-closed）。
    let Some(office) = &state.office else {
        return Err(ApiError::NotFound);
    };
    // viewer check（StorageService 経由・監査つき）。権限なしは存在秘匿の 404。
    let node = state
        .storage
        .get_metadata(&ctx, req.file_id, trace.0.as_deref())
        .await
        .map_err(|e| match ApiError::from(e) {
            ApiError::Forbidden => ApiError::NotFound,
            other => other,
        })?;
    if node.kind != NodeKind::File {
        return Err(ApiError::NotFound);
    }
    let ext = node
        .name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    let action_url = match office.suite.editor_action_url(&ext).await {
        Ok(Some(url)) => url,
        // 未対応拡張子は「編集面が存在しない」＝404（ドライブ側はダウンロードへ誘導）。
        Ok(None) => return Err(ApiError::NotFound),
        // discovery 失敗は機能 off の fail-closed（Collabora 停止等の一時障害）。
        Err(office::OfficeError::Discovery(detail)) => {
            return Err(ApiError::ServiceUnavailable(format!(
                "office discovery: {detail}"
            )));
        }
        Err(e) => return Err(ApiError::Internal(format!("office: {e}"))),
    };
    let access_token = office::wopi::token::issue(&office.wopi.token_key, &ctx, req.file_id)
        .map_err(|e| ApiError::Internal(format!("office token: {e}")))?;
    Ok(Json(OfficeSessionResponse {
        action_url,
        wopi_src: format!("{}/wopi/files/{}", office.wopi_base_url, req.file_id),
        access_token,
        access_token_ttl_ms: office::TOKEN_TTL.as_millis() as u64,
    }))
}

/// チャットの図をグラフとして文書へ入れるリクエスト（#731）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct InsertOfficeChartRequest {
    /// 入れる先の Excel（xlsx/ods）か Word（docx/odt）。
    pub file_id: Uuid,
    pub chart: office::engine::chart_insert::ChartInsert,
}

/// グラフを入れた結果。
#[derive(Debug, Serialize, ToSchema)]
pub struct InsertOfficeChartResponse {
    /// Excel のとき、表とグラフを置いた新しいシートの名前（Word は null）。
    pub sheet: Option<String>,
}

/// チャットの図を、開いている Excel・Word へ編集できるグラフとして入れる（#731）。
///
/// Excel は新しいシートに表を書いてその右にグラフを置き、Word は文書の末尾にグラフを入れる。
/// AI の文書操作と同じエンジンを通るので、editor 権限の確認・全部成功か全部取り消し・人の
/// 1 回の「元に戻す」も同じ。
#[utoipa::path(
    post, path = "/office/charts", request_body = InsertOfficeChartRequest,
    responses(
        (status = 200, description = "入れた", body = InsertOfficeChartResponse),
        (status = 400, description = "グラフのデータが不正・この形式には入れられない（理由は本文の message）"),
        (status = 401, description = "未認証"),
        (status = 403, description = "編集の権限がない"),
        (status = 404, description = "Office が無効"),
        (status = 409, description = "この文書で別の AI 編集が進行中"),
        (status = 503, description = "Office に接続できない"),
    ),
    security(("session" = [])),
)]
pub async fn insert_office_chart(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Json(req): Json<InsertOfficeChartRequest>,
) -> Result<Json<InsertOfficeChartResponse>, ApiError> {
    use office::engine::OfficeEngineError as E;
    use office::live::LiveEditError as L;
    let Some(office) = &state.office else {
        return Err(ApiError::NotFound);
    };
    let label = format!(
        "グラフを挿入「{}」",
        req.chart.title.as_deref().unwrap_or("グラフ")
    );
    match office
        .engine
        .insert_chart(&ctx, req.file_id, &label, &req.chart)
        .await
    {
        Ok(r) if r.applied => Ok(Json(InsertOfficeChartResponse { sheet: r.sheet })),
        // エンジンが丸ごと取り消した（文書は変わっていない）。理由の行だけを返す。
        Ok(r) => Err(rejected(&cause_of(&r.text))),
        Err(E::Invalid(e)) => Err(rejected(&e.message)),
        Err(E::Unsupported | E::Session(L::Unsupported)) => Err(rejected(
            "この文書にはグラフを入れられません（Excel・Word のみ）",
        )),
        Err(E::Session(L::Denied)) => Err(ApiError::Forbidden),
        Err(E::Session(L::Busy)) => Err(ApiError::Conflict),
        Err(e @ (E::Session(_) | E::Engine(_))) => {
            Err(ApiError::ServiceUnavailable(format!("office: {e}")))
        }
        Err(E::Range(e)) => Err(ApiError::Internal(format!("office: {e}"))),
    }
}

/// 入れられなかった理由を本文に載せた 400（画面がそのまま見せる）。
fn rejected(message: &str) -> ApiError {
    tracing::info!(reason = %message, "グラフを入れられなかった（文書は変わっていない）");
    ApiError::UnprocessableJson(serde_json::json!({
        "status": 400, "title": "Bad Request", "message": message,
    }))
}

/// エンジンの失敗の報告から「原因: …」の行を取り出す（無ければ全体の 1 行目）。
fn cause_of(text: &str) -> String {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("原因:").map(str::trim))
        .or_else(|| text.lines().next())
        .unwrap_or("グラフを入れられませんでした")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cause_is_the_reason_line() {
        let text = "適用できませんでした。途中までの変更もすべて取り消したので、文書は変わっていません。\n原因: 2 件目の操作（add_chart）・シート「x」がありません";
        assert_eq!(
            cause_of(text),
            "2 件目の操作（add_chart）・シート「x」がありません"
        );
        assert_eq!(cause_of("だめでした"), "だめでした");
    }
}
