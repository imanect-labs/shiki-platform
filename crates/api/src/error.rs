//! API エラー型と HTTP レスポンスへの変換（ProblemDetails 風）。

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("認証が必要です")]
    Unauthorized,
    #[error("権限がありません")]
    Forbidden,
    #[error("対象が見つかりません")]
    NotFound,
    #[error("競合しています")]
    Conflict,
    /// 構造化 409 ボディ（scope 拡大の missing_scopes 等・詳細ペイロードを保つ）。
    #[error("競合しています")]
    ConflictJson(serde_json::Value),
    #[error("不正なリクエスト: {0}")]
    BadRequest(String),
    /// 検証エラー等の**構造化 400 ボディ**をそのまま返す（per-node/per-edge エラー等）。
    /// `BadRequest` は body が `{status,title}` に潰れるため、詳細ペイロードにはこちらを使う。
    #[error("検証エラー")]
    UnprocessableJson(serde_json::Value),
    /// 422（構造化した拒否理由をそのまま返す・SQL 前段面の入口の拒否）。
    #[error("処理できません")]
    Unprocessable422(serde_json::Value),
    /// 機能が無効/未準備（RAG 無効設定など）。理由はログのみ（クライアントへ漏らさない）。
    #[error("利用できません: {0}")]
    ServiceUnavailable(String),
    /// 利用上限・レート制限（429）。文言は**そのままユーザーへ出せる**ものだけを入れる
    /// （会話に表示される・llm-gateway 側で整形済み）。
    #[error("{0}")]
    RateLimited(String),
    #[error("内部エラー: {0}")]
    Internal(String),
    /// 保持期間を過ぎた位置の再開（スナップショット読取の基準位置など）。最初から取り直す。
    #[error("保持期間を過ぎました")]
    Gone,
    /// 本文が上限を超えた（構造化 413・一括取込の `import_too_large` 等）。
    #[error("本文が大きすぎます")]
    PayloadTooLargeJson(serde_json::Value),
    /// 再試行できる 503（構造化本文 ＋ `Retry-After` 秒・statement timeout の `read_timeout` 等）。
    #[error("一時的に利用できません")]
    RetryLaterJson {
        body: serde_json::Value,
        retry_after_secs: u64,
    },
}

/// 再試行できる 503 の `Retry-After`（秒）。
pub const RETRY_AFTER_SECS: u64 = 2;

impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiError::Forbidden => StatusCode::FORBIDDEN,
            ApiError::NotFound => StatusCode::NOT_FOUND,
            ApiError::Conflict | ApiError::ConflictJson(_) => StatusCode::CONFLICT,
            ApiError::BadRequest(_) | ApiError::UnprocessableJson(_) => StatusCode::BAD_REQUEST,
            ApiError::Unprocessable422(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::ServiceUnavailable(_) | ApiError::RetryLaterJson { .. } => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            ApiError::RateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::Gone => StatusCode::GONE,
            ApiError::PayloadTooLargeJson(_) => StatusCode::PAYLOAD_TOO_LARGE,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        // 内部エラーは詳細をクライアントに漏らさず、ログにのみ残す。
        if let ApiError::Internal(ref detail) = self {
            tracing::error!(error = %detail, "内部エラー");
        }
        // 503 の原因（RAG worker/Qdrant 障害等）も追跡できるようログへ残す。
        if let ApiError::ServiceUnavailable(ref detail) = self {
            tracing::warn!(error = %detail, "サービス利用不可（503）");
        }
        if let ApiError::RetryLaterJson {
            body,
            retry_after_secs,
        } = self
        {
            let mut resp = (status, Json(body)).into_response();
            if let Ok(v) = axum::http::HeaderValue::from_str(&retry_after_secs.to_string()) {
                resp.headers_mut()
                    .insert(axum::http::header::RETRY_AFTER, v);
            }
            return resp;
        }
        // 構造化ボディはそのまま返す（検証エラーの per-node/per-edge 詳細を保つ）。
        if let ApiError::UnprocessableJson(payload)
        | ApiError::ConflictJson(payload)
        | ApiError::Unprocessable422(payload)
        | ApiError::PayloadTooLargeJson(payload) = self
        {
            return (status, Json(payload)).into_response();
        }
        // 429 だけは文言を `detail` に載せる。上限額と再開の目安を含み、**そのまま出せる**
        // ものに限られている（他の種別は上流の詳細を含むため載せない）。
        if let ApiError::RateLimited(ref detail) = self {
            let body = Json(json!({
                "status": status.as_u16(),
                "title": status.canonical_reason().unwrap_or("Error"),
                "detail": detail,
            }));
            return (status, body).into_response();
        }
        let body = Json(json!({
            "status": status.as_u16(),
            "title": status.canonical_reason().unwrap_or("Error"),
        }));
        (status, body).into_response()
    }
}

impl From<authz::AuthzError> for ApiError {
    fn from(err: authz::AuthzError) -> Self {
        ApiError::Internal(format!("authz: {err}"))
    }
}

impl From<storage::StorageError> for ApiError {
    fn from(err: storage::StorageError) -> Self {
        use storage::StorageError as SE;
        match err {
            SE::Forbidden => ApiError::Forbidden,
            SE::NotFound => ApiError::NotFound,
            SE::Conflict => ApiError::Conflict,
            // 版の衝突は今の版を返す（クライアントが「最新を開く／上書き」を選べるように）。
            SE::VersionConflict { current } => ApiError::ConflictJson(json!({
                "status": 409,
                "title": "Conflict",
                "code": "version_conflict",
                "current_version": current,
            })),
            // Invalid と整合性エラー（宣言ハッシュ不一致・staging 未完了等）はどちらも
            // クライアント起因の 400。
            SE::Invalid(msg) | SE::Integrity(msg) => ApiError::BadRequest(msg),
            SE::ObjectStore(e) => ApiError::Internal(format!("object_store: {e}")),
            SE::Db(e) => ApiError::Internal(format!("db: {e}")),
            SE::Authz(e) => ApiError::Internal(format!("authz: {e}")),
        }
    }
}

impl From<rag::RagError> for ApiError {
    fn from(err: rag::RagError) -> Self {
        use rag::RagError as RE;
        match err {
            // worker/Qdrant への到達失敗など一時障害は 503（クライアントは再試行できる）。
            RE::Http(_) | RE::Worker(_) | RE::Vector(_) => {
                ApiError::ServiceUnavailable(format!("rag: {err}"))
            }
            other => ApiError::Internal(format!("rag: {other}")),
        }
    }
}

impl From<chat::ChatError> for ApiError {
    fn from(err: chat::ChatError) -> Self {
        use chat::ChatError as CE;
        match err {
            CE::NotFound => ApiError::NotFound,
            CE::Forbidden => ApiError::Forbidden,
            CE::Invalid(msg) => ApiError::BadRequest(msg),
            CE::Unavailable(msg) => ApiError::ServiceUnavailable(format!("chat: {msg}")),
            // 整形済みの文言をそのまま返す（`chat:` を前置しない・会話に表示されるため）。
            CE::RateLimited(msg) => ApiError::RateLimited(msg),
            CE::Internal(msg) => ApiError::Internal(format!("chat: {msg}")),
        }
    }
}

impl From<artifact::ArtifactError> for ApiError {
    fn from(err: artifact::ArtifactError) -> Self {
        use artifact::ArtifactError as AE;
        match err {
            AE::NotFound => ApiError::NotFound,
            AE::Forbidden => ApiError::Forbidden,
            AE::Invalid(msg) => ApiError::BadRequest(msg),
            AE::Conflict(_) => ApiError::Conflict,
            AE::Internal(msg) => ApiError::Internal(format!("artifact: {msg}")),
        }
    }
}

impl From<data::DataError> for ApiError {
    fn from(err: data::DataError) -> Self {
        use crate::error_body::{
            to_body, BadRequestBody, ConflictBody, ConflictCode, RetryLaterBody,
        };
        use data::DataError as DE;
        let conflict = |b: ConflictBody| ApiError::ConflictJson(to_body(&b));
        match err {
            DE::NotFound => ApiError::NotFound,
            DE::Forbidden => ApiError::Forbidden,
            // 理由を構造化していない入力の誤りは汎用の文言（理由はログだけ・値や読めない列名を返さない）。
            DE::Invalid(msg) => {
                tracing::info!(reason = %msg, "data invalid (400)");
                ApiError::UnprocessableJson(to_body(&BadRequestBody::new(
                    data::InvalidCode::InvalidInput,
                    data::InvalidCode::InvalidInput.default_message(),
                    None,
                )))
            }
            // 構造化した入力の誤り（code・利用者に見せてよい文言・対象の列の参照名）。
            DE::InvalidInput(e) => ApiError::UnprocessableJson(to_body(&BadRequestBody::new(
                e.code(),
                e.message(),
                e.field_key(),
            ))),
            // 409 はクライアントが「他の人が先に更新した（rev）」「同じ値が既にある（unique）」
            // 「参照先の変更を受けて再計算中」「同名のテーブル」「その他」を出し分けられるよう `code` で
            // 構造化する（値・相手の行・内部キーは含めない。理由の文言はログにだけ残す）。
            DE::Conflict(reason) => {
                tracing::info!(%reason, "data conflict (409)");
                conflict(ConflictBody::new(ConflictCode::Conflict))
            }
            DE::NameTaken => conflict(ConflictBody::new(ConflictCode::NameTaken)),
            DE::RevConflict { current } => conflict(ConflictBody {
                current_rev: Some(current),
                ..ConflictBody::new(ConflictCode::RevConflict)
            }),
            DE::UniqueViolation => conflict(ConflictBody::new(ConflictCode::UniqueViolation)),
            DE::DerivedRecomputing { field } => conflict(ConflictBody {
                field: Some(field),
                ..ConflictBody::new(ConflictCode::DerivedRecomputing)
            }),
            DE::Internal(msg) => ApiError::Internal(format!("data: {msg}")),
            DE::Gone(reason) => {
                tracing::info!(%reason, "data gone (410)");
                ApiError::Gone
            }
            // 索引の準備中・承認待ちは UI が「準備中」「承認が要る」を出し分けられるよう構造化する
            // （値・内部キー・件数を含めない）。
            DE::IndexPreparing { field, progress } => conflict(ConflictBody {
                field: Some(field),
                progress: Some(progress),
                ..ConflictBody::new(ConflictCode::IndexPreparing)
            }),
            DE::IndexApprovalRequired {
                field,
                indexed,
                cap,
            } => conflict(ConflictBody {
                field: Some(field),
                indexed_fields: Some(indexed),
                max_indexed_fields: Some(cap),
                ..ConflictBody::new(ConflictCode::IndexApprovalRequired)
            }),
            // 受付制御（予測コストが読取予算を超える）。提案の語彙だけで、推定値・予測コスト・
            // 計画名を含めない（PIT-75）。
            DE::QueryTooExpensive { suggestions } => {
                ApiError::UnprocessableJson(serde_json::json!({
                    "status": 400,
                    "title": "Bad Request",
                    "code": "query_too_expensive",
                    "suggestions": suggestions.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                }))
            }
            // テナントの同時実行の枠（再試行可）。文言は固定で、推定値を含まない（PIT-75）。
            DE::ReadOverloaded => {
                ApiError::RateLimited("混み合っています。しばらくしてから再試行してください".into())
            }
            // statement timeout（再試行可・SQL や推定値は返さない）。
            DE::ReadTimeout => ApiError::RetryLaterJson {
                body: to_body(&RetryLaterBody {
                    status: 503,
                    title: "Service Unavailable".into(),
                    code: "read_timeout".into(),
                    message: "読取が時間内に終わりませんでした。しばらくしてから再試行してください"
                        .into(),
                }),
                retry_after_secs: RETRY_AFTER_SECS,
            },
            // 入口での拒否（SQL 前段面の部分集合外・名前解決不能・資源上限）。422 と構造化した理由
            // （LLM が自己修正できる・存在しない列と見えない列は同一の本文・data-platform §16.1）。
            DE::Rejected(r) => {
                let mut body = serde_json::to_value(*r).unwrap_or_else(|_| serde_json::json!({}));
                if let Some(o) = body.as_object_mut() {
                    o.insert("status".into(), serde_json::json!(422));
                    o.insert("title".into(), serde_json::json!("Unprocessable Entity"));
                }
                ApiError::Unprocessable422(body)
            }
        }
    }
}

impl From<app_platform::AppPlatformError> for ApiError {
    fn from(err: app_platform::AppPlatformError) -> Self {
        use app_platform::AppPlatformError as E;
        match err {
            E::NotFound => ApiError::NotFound,
            E::Forbidden => ApiError::Forbidden,
            E::Invalid(msg) => ApiError::BadRequest(msg),
            E::Conflict(_) => ApiError::Conflict,
            E::Internal(msg) => ApiError::Internal(format!("app-platform: {msg}")),
        }
    }
}

impl From<secrets::SecretError> for ApiError {
    fn from(err: secrets::SecretError) -> Self {
        use secrets::SecretError as SE;
        match err {
            SE::NotFound => ApiError::NotFound,
            SE::Forbidden => ApiError::Forbidden,
            SE::Invalid(msg) => ApiError::BadRequest(msg),
            SE::DestinationDenied(msg) => ApiError::BadRequest(format!("destination: {msg}")),
            SE::Conflict(_) => ApiError::Conflict,
            // 暗号エラーの詳細はクライアントへ漏らさない（内部扱い）。
            SE::Crypto(_) | SE::Internal(_) => ApiError::Internal("secret: 内部エラー".into()),
        }
    }
}

impl From<llm_gateway::LlmError> for ApiError {
    fn from(err: llm_gateway::LlmError) -> Self {
        use llm_gateway::LlmError as LE;
        match err {
            LE::Unavailable(msg) => ApiError::ServiceUnavailable(format!("llm: {msg}")),
            // 上限超過は**そのまま出せる文言**（provider.rs で整形済み）。`llm:` 等の接頭辞を
            // 付けない — この文字列は会話にそのまま表示される。
            LE::RateLimited(msg) => ApiError::RateLimited(msg),
            LE::BadRequest(msg) => ApiError::BadRequest(msg),
            LE::Config(msg) | LE::Internal(msg) => ApiError::Internal(format!("llm: {msg}")),
        }
    }
}

impl From<crate::session::SessionError> for ApiError {
    fn from(err: crate::session::SessionError) -> Self {
        ApiError::Internal(format!("session: {err}"))
    }
}

impl From<crate::oidc::OidcError> for ApiError {
    fn from(err: crate::oidc::OidcError) -> Self {
        match err {
            // token エンドポイントの 4xx（invalid_grant / 失効 refresh 等）は認証失敗扱い。
            crate::oidc::OidcError::Status { status, .. } if (400..500).contains(&status) => {
                tracing::debug!(%status, "OIDC token エンドポイントが 4xx（認証失敗）");
                ApiError::Unauthorized
            }
            other => ApiError::Internal(format!("oidc: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[test]
    fn status_maps_each_variant() {
        assert_eq!(ApiError::Unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(ApiError::Forbidden.status(), StatusCode::FORBIDDEN);
        assert_eq!(ApiError::NotFound.status(), StatusCode::NOT_FOUND);
        assert_eq!(ApiError::Conflict.status(), StatusCode::CONFLICT);
        assert_eq!(
            ApiError::BadRequest("x".into()).status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ApiError::Internal("boom".into()).status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn from_storage_error_maps_status() {
        use storage::StorageError as SE;
        assert!(matches!(ApiError::from(SE::Forbidden), ApiError::Forbidden));
        assert!(matches!(ApiError::from(SE::NotFound), ApiError::NotFound));
        assert!(matches!(ApiError::from(SE::Conflict), ApiError::Conflict));
        match ApiError::from(SE::VersionConflict { current: 7 }) {
            ApiError::ConflictJson(body) => {
                assert_eq!(body["code"], "version_conflict");
                assert_eq!(body["current_version"], 7);
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(matches!(
            ApiError::from(SE::Invalid("bad".into())),
            ApiError::BadRequest(_)
        ));
        assert!(matches!(
            ApiError::from(SE::Integrity("mismatch".into())),
            ApiError::BadRequest(_)
        ));
    }

    /// レスポンスボディを ProblemDetails 風 JSON として読み出すヘルパ。
    async fn body_json(err: ApiError) -> (StatusCode, serde_json::Value) {
        let resp = err.into_response();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (status, value)
    }

    /// data の 409 は `code` で種類を出し分ける（rev 競合・unique 違反・再計算中・その他・監査 B1-6）。
    #[tokio::test]
    async fn data_conflicts_are_structured_by_code() {
        use data::DataError as DE;
        let (status, body) = body_json(ApiError::from(DE::RevConflict { current: 4 })).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "rev_conflict");
        assert_eq!(body["current_rev"], 4);
        let (_, body) = body_json(ApiError::from(DE::UniqueViolation)).await;
        assert_eq!(body["code"], "unique_violation");
        assert_eq!(
            body.as_object().unwrap().len(),
            3,
            "値や相手の行を含めない: {body}"
        );
        let (_, body) = body_json(ApiError::from(DE::DerivedRecomputing {
            field: "total".into(),
        }))
        .await;
        assert_eq!(body["code"], "derived_recomputing");
        assert_eq!(body["field"], "total");
        let (status, body) = body_json(ApiError::from(DE::Conflict(
            "テーブル名 'x' は既に存在します".into(),
        )))
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "conflict");
        assert!(body.get("detail").is_none(), "理由の文言は返さない: {body}");
    }

    /// data の 400 は構造化（code・安全な文言・列の参照名）。理由を構造化していない誤りは汎用の文言で、
    /// 内部の理由（値・列名を含み得る）を本文に出さない（検証 C-P2-4）。
    #[tokio::test]
    async fn data_bad_requests_are_structured_and_safe() {
        use data::{DataError as DE, InvalidCode};
        let (status, body) = body_json(ApiError::from(DE::input_field(
            InvalidCode::ValueTooLong,
            "memo",
        )))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "value_too_long");
        assert_eq!(body["field"], "memo");
        assert_eq!(body["message"], InvalidCode::ValueTooLong.default_message());
        let (status, body) = body_json(ApiError::from(DE::Invalid(
            "'salary' の値 'secret-123' は不正です".into(),
        )))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "invalid_input");
        let text = body.to_string();
        assert!(
            !text.contains("salary") && !text.contains("secret-123"),
            "{text}"
        );
        assert!(body.get("field").is_none());
        let (status, body) = body_json(ApiError::from(DE::NameTaken)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "name_taken");
    }

    /// statement timeout は再試行できる 503（`read_timeout`・`Retry-After`）。
    #[tokio::test]
    async fn read_timeout_is_retryable_503() {
        let resp = ApiError::from(data::DataError::ReadTimeout).into_response();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("2")
        );
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["code"], "read_timeout");
    }

    /// SQL 前段面の入口の拒否は 422 と構造化した理由（code / message / location / limit / expected）を
    /// 本文の最上位にそのまま返す（LLM の自己修正に使う・Task 15.10）。
    #[tokio::test]
    async fn query_rejection_is_422_with_structured_reason() {
        let r: data::QueryRejection = serde_json::from_value(serde_json::json!({
            "code": "limit_exceeded",
            "message": "クエリが入口の上限 max_ast_depth（32）を超えています",
            "location": {"line": 1, "column": 30},
            "limit": {"name": "max_ast_depth", "max": 32},
        }))
        .unwrap();
        let (status, body) =
            body_json(ApiError::from(data::DataError::Rejected(Box::new(r)))).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["status"], 422);
        assert_eq!(body["code"], "limit_exceeded");
        assert_eq!(body["limit"]["name"], "max_ast_depth");
        assert_eq!(body["location"]["line"], 1);
        let r: data::QueryRejection = serde_json::from_value(serde_json::json!({
            "code": "unknown_name",
            "message": "列 'salary' は見つからないか、参照できません",
            "expected": ["_id", "name"],
        }))
        .unwrap();
        let (status, body) =
            body_json(ApiError::from(data::DataError::Rejected(Box::new(r)))).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["code"], "unknown_name");
        assert_eq!(body["expected"], serde_json::json!(["_id", "name"]));
    }

    /// 受付制御（重すぎるクエリ）は提案の語彙だけの構造化 400、枠の不足は 429。推定値・予測コスト・
    /// 計画名は本文に出ない（PIT-75）。
    #[tokio::test]
    async fn query_admission_errors_carry_no_estimates() {
        let (status, body) = body_json(ApiError::from(data::DataError::QueryTooExpensive {
            suggestions: vec![
                data::QuerySuggestion::NarrowFilter,
                data::QuerySuggestion::PromoteIndex,
                data::QuerySuggestion::CompositeIndex,
            ],
        }))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "query_too_expensive");
        assert_eq!(
            body["suggestions"],
            serde_json::json!(["narrow_filter", "promote_index", "composite_index"])
        );
        let text = body.to_string();
        for banned in ["cost", "estimate", "strategy", "rows", "ms"] {
            assert!(!text.contains(banned), "{banned}: {text}");
        }
        let (status, _) = body_json(ApiError::from(data::DataError::ReadOverloaded)).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn unauthorized_into_response() {
        let (status, body) = body_json(ApiError::Unauthorized).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["status"], 401);
        assert_eq!(body["title"], "Unauthorized");
    }

    #[tokio::test]
    async fn forbidden_into_response() {
        let (status, body) = body_json(ApiError::Forbidden).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["status"], 403);
        assert_eq!(body["title"], "Forbidden");
    }

    #[tokio::test]
    async fn internal_into_response_hides_detail() {
        // 内部エラーの詳細はボディに漏らさない（status/title のみ）。
        let (status, body) = body_json(ApiError::Internal("secret detail".into())).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["status"], 500);
        assert_eq!(body["title"], "Internal Server Error");
        assert!(!body.to_string().contains("secret detail"));
    }

    #[test]
    fn from_authz_error_is_internal() {
        let api: ApiError = authz::AuthzError::InvalidModel("fga down".into()).into();
        assert!(matches!(api, ApiError::Internal(_)));
    }

    #[test]
    fn from_session_error_is_internal() {
        let api: ApiError = crate::session::SessionError::Backend("redis down".into()).into();
        assert!(matches!(api, ApiError::Internal(_)));
    }

    #[test]
    fn from_oidc_4xx_is_unauthorized() {
        // token エンドポイント 4xx（invalid_grant 等）は認証失敗にマップする。
        let err = crate::oidc::OidcError::Status {
            status: 400,
            body: "invalid_grant".into(),
        };
        assert!(matches!(ApiError::from(err), ApiError::Unauthorized));
    }

    #[test]
    fn from_oidc_5xx_is_internal() {
        // 5xx は一過性障害として内部エラー扱い。
        let err = crate::oidc::OidcError::Status {
            status: 503,
            body: "down".into(),
        };
        assert!(matches!(ApiError::from(err), ApiError::Internal(_)));
    }

    #[test]
    fn from_oidc_transport_is_internal() {
        // 到達失敗（transport）も内部エラー扱い。
        let err = crate::oidc::OidcError::Transport("connection refused".into());
        assert!(matches!(ApiError::from(err), ApiError::Internal(_)));
    }

    #[test]
    fn error_display_is_safe_messages() {
        // Display 文言（ログ/内部用）が想定どおりであること。
        assert_eq!(ApiError::Unauthorized.to_string(), "認証が必要です");
        assert_eq!(ApiError::Forbidden.to_string(), "権限がありません");
        assert_eq!(ApiError::Internal("x".into()).to_string(), "内部エラー: x");
    }

    #[tokio::test]
    async fn rate_limited_returns_429_with_the_message_in_detail() {
        use axum::body::to_bytes;
        // 429 の文言は上限額と再開の目安を含み、そのままユーザーへ出せる（llm-gateway が整形）。
        const MSG: &str =
            "プレビュー環境の今月の利用上限（$5.00）に達しました。翌月 1 日にリセットされます。";
        let res = ApiError::RateLimited(MSG.into()).into_response();
        assert_eq!(res.status(), axum::http::StatusCode::TOO_MANY_REQUESTS);
        let body = to_bytes(res.into_body(), 64 * 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["status"], 429);
        assert_eq!(v["detail"], MSG);
    }

    #[tokio::test]
    async fn other_errors_do_not_leak_detail_into_the_body() {
        use axum::body::to_bytes;
        // 429 以外は上流の詳細を含み得るので、ボディへ載せない（ログにのみ残す）。
        let res = ApiError::ServiceUnavailable("qdrant: connection refused".into()).into_response();
        let body = to_bytes(res.into_body(), 64 * 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v.get("detail").is_none(), "{v}");
        assert_eq!(v["status"], 503);
    }
}
