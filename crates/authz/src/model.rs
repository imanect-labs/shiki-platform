//! authorization model の冪等ロードとバージョン管理。
//!
//! OpenFGA の model はイミュータブル追記。起動毎に write するとバージョンが
//! 無限に増えるため、「現行最新 model が期待 model と意味的に一致するか」を
//! 比較し、差分があるときだけ新バージョンを書き込む。
//! model 定義は `model/authorization-model.json` を正本として人がレビューする
//! （`.fga` DSL を併置し可読性を確保）。

use serde_json::Value;

use crate::{error::AuthzError, fga_http::FgaHttp};

/// レビュー済みの authorization model 正本（JSON）。`.fga` 併置版と同期させる。
pub const AUTHORIZATION_MODEL_JSON: &str = include_str!("../model/authorization-model.json");

/// 正本 model を [`Value`] として読み込む。
pub fn default_model() -> Value {
    serde_json::from_str(AUTHORIZATION_MODEL_JSON)
        .expect("同梱の authorization-model.json は妥当な JSON であること")
}

/// store と authorization model を冪等に用意し、`(store_id, model_id)` を返す。
///
/// - `store_name` の store が無ければ作成する。
/// - 最新 model が `desired_model` と意味的に一致すれば再利用、しなければ書き込む。
pub async fn ensure_store_and_model(
    fga: &FgaHttp,
    store_name: &str,
    desired_model: &Value,
) -> Result<(String, String), AuthzError> {
    let store_id = match fga.find_store(store_name).await? {
        Some(id) => {
            tracing::info!(store = store_name, store_id = %id, "既存の OpenFGA store を利用");
            id
        }
        None => {
            let id = fga.create_store(store_name).await?;
            tracing::info!(store = store_name, store_id = %id, "OpenFGA store を新規作成");
            id
        }
    };

    let desired_fp = model_fingerprint(desired_model);

    if let Some(latest_id) = fga.latest_model_id(&store_id).await? {
        let current = fga.get_model(&store_id, &latest_id).await?;
        if model_fingerprint(&current) == desired_fp {
            tracing::info!(model_id = %latest_id, "authorization model は最新と一致（書き込みスキップ）");
            return Ok((store_id, latest_id));
        }
        tracing::info!("authorization model に差分あり、新バージョンを書き込みます");
    }

    let model_id = fga.write_model(&store_id, desired_model).await?;
    tracing::info!(model_id = %model_id, "authorization model を書き込み");
    Ok((store_id, model_id))
}

/// 比較用に model の意味的な核（schema_version / type_definitions / conditions）を抽出する。
/// `id` など書き込み後にしか付かないフィールドを除外して冪等比較を可能にする。
fn model_fingerprint(model: &Value) -> Value {
    serde_json::json!({
        "schema_version": model.get("schema_version").cloned().unwrap_or(Value::Null),
        "type_definitions": model.get("type_definitions").cloned().unwrap_or(Value::Null),
        "conditions": model.get("conditions").cloned().unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_ignores_id() {
        let with_id = serde_json::json!({
            "id": "01ABC",
            "schema_version": "1.1",
            "type_definitions": [{"type": "user"}],
        });
        let without_id = serde_json::json!({
            "schema_version": "1.1",
            "type_definitions": [{"type": "user"}],
        });
        assert_eq!(model_fingerprint(&with_id), model_fingerprint(&without_id));
    }
}
