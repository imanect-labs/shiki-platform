//! authorization model の冪等ロードとバージョン管理。
//!
//! OpenFGA の model はイミュータブル追記。起動毎に write するとバージョンが
//! 無限に増えるため、「現行最新 model が期待 model と意味的に一致するか」を
//! 比較し、差分があるときだけ新バージョンを書き込む。
//! model 定義は `model/authorization-model.json` を正本として人がレビューする
//! （`.fga` DSL を併置し可読性を確保）。両者の userset 構造が乖離しないことは
//! `fga_dsl` の drift テストが CI で保証する（#66）。

use serde_json::Value;

use crate::{error::AuthzError, fga_http::FgaHttp};

/// レビュー済みの authorization model 正本（JSON）。`.fga` 併置版と同期させる。
pub const AUTHORIZATION_MODEL_JSON: &str = include_str!("../model/authorization-model.json");

/// 正本 model を [`Value`] として読み込む。
// `include_str!` で同梱した JSON のパースであり、失敗はビルド時に固定される
// プログラミング不変条件（実行時入力ではない）。`expect` で即時検知する。
#[allow(clippy::expect_used)]
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
    let store_id = if let Some(id) = fga.find_store(store_name).await? {
        tracing::info!(store = store_name, store_id = %id, "既存の OpenFGA store を利用");
        id
    } else {
        let id = fga.create_store(store_name).await?;
        tracing::info!(store = store_name, store_id = %id, "OpenFGA store を新規作成");
        id
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

    #[test]
    fn default_model_is_valid_json_object() {
        // 同梱の正本 model はパース可能でオブジェクトであること。
        let model = default_model();
        assert!(model.is_object());
    }

    #[test]
    fn default_model_has_expected_shape() {
        // 正本 model は schema_version と type_definitions を持つこと。
        let model = default_model();
        assert_eq!(
            model.get("schema_version").and_then(|v| v.as_str()),
            Some("1.1")
        );
        let types = model
            .get("type_definitions")
            .and_then(|v| v.as_array())
            .expect("type_definitions は配列");
        // user / organization / role が定義されていること。
        let type_names: Vec<&str> = types
            .iter()
            .filter_map(|t| t.get("type").and_then(|v| v.as_str()))
            .collect();
        assert!(type_names.contains(&"user"));
        assert!(type_names.contains(&"organization"));
        assert!(type_names.contains(&"role"));
        // Phase 1（ストレージ）で folder / file を追加した。
        assert!(type_names.contains(&"folder"));
        assert!(type_names.contains(&"file"));
    }

    #[test]
    fn fga_and_json_declare_same_types() {
        // `.fga`（人がレビュー）と `.json`（実際に投入）の type 名集合が一致することを CI で保証する
        // （どちらか片方にだけ type を足す drift を検出する。認可はチョークポイントゆえの安価な保険）。
        use std::collections::BTreeSet;
        let fga = include_str!("../model/authorization-model.fga");
        let fga_types: BTreeSet<String> = fga
            .lines()
            .filter_map(|line| line.trim().strip_prefix("type "))
            .filter_map(|rest| rest.split_whitespace().next())
            .map(str::to_string)
            .collect();
        let model = default_model();
        let json_types: BTreeSet<String> = model
            .get("type_definitions")
            .and_then(|v| v.as_array())
            .expect("type_definitions は配列")
            .iter()
            .filter_map(|t| t.get("type").and_then(|v| v.as_str()))
            .map(str::to_string)
            .collect();
        assert_eq!(
            fga_types, json_types,
            ".fga と .json の type 名集合が一致すること（model drift 検出）"
        );
    }

    #[test]
    fn storage_types_have_expected_relations() {
        // folder / file は owner / editor / viewer / parent を持つこと（厳格モデル）。
        let model = default_model();
        let types = model
            .get("type_definitions")
            .and_then(|v| v.as_array())
            .expect("type_definitions は配列");
        for type_name in ["folder", "file"] {
            let def = types
                .iter()
                .find(|t| t.get("type").and_then(|v| v.as_str()) == Some(type_name))
                .unwrap_or_else(|| panic!("{type_name} 型が定義されていること"));
            let relations = def
                .get("relations")
                .and_then(|v| v.as_object())
                .expect("relations はオブジェクト");
            for rel in ["parent", "owner", "editor", "viewer"] {
                assert!(
                    relations.contains_key(rel),
                    "{type_name} は relation {rel} を持つこと"
                );
            }
        }
    }

    #[test]
    fn storage_editor_viewer_accept_role_member() {
        // #76: folder / file の editor・viewer は user に加え role#member を共有先として受理すること。
        // owner は role#member を受理しない（共有語彙は viewer/editor のみ・owner 横展開の禁止）。
        let model = default_model();
        let types = model
            .get("type_definitions")
            .and_then(|v| v.as_array())
            .expect("type_definitions は配列");
        let accepts = |type_name: &str, rel: &str, want_type: &str, want_rel: Option<&str>| {
            let def = types
                .iter()
                .find(|t| t.get("type").and_then(|v| v.as_str()) == Some(type_name))
                .unwrap();
            def.get("metadata")
                .and_then(|m| m.get("relations"))
                .and_then(|r| r.get(rel))
                .and_then(|r| r.get("directly_related_user_types"))
                .and_then(|v| v.as_array())
                .expect("directly_related_user_types は配列")
                .iter()
                .any(|t| {
                    t.get("type").and_then(|v| v.as_str()) == Some(want_type)
                        && t.get("relation").and_then(|v| v.as_str()) == want_rel
                })
        };
        for type_name in ["folder", "file"] {
            for rel in ["editor", "viewer"] {
                assert!(
                    accepts(type_name, rel, "user", None),
                    "{type_name}.{rel} は user を受理すること"
                );
                assert!(
                    accepts(type_name, rel, "role", Some("member")),
                    "{type_name}.{rel} は role#member を受理すること（#76）"
                );
            }
            // owner は role#member を受理しない。
            assert!(
                !accepts(type_name, "owner", "role", Some("member")),
                "{type_name}.owner は role#member を受理しないこと"
            );
        }
    }

    #[test]
    fn thread_type_has_share_relations() {
        // #37: thread は owner / editor / commenter / viewer を持ち、
        // editor / commenter / viewer は user と role#member を共有先として受理すること。
        let model = default_model();
        let types = model
            .get("type_definitions")
            .and_then(|v| v.as_array())
            .expect("type_definitions は配列");
        let thread = types
            .iter()
            .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("thread"))
            .expect("thread 型が定義されていること");
        let relations = thread
            .get("relations")
            .and_then(|v| v.as_object())
            .expect("relations はオブジェクト");
        for rel in ["owner", "editor", "commenter", "viewer"] {
            assert!(
                relations.contains_key(rel),
                "thread は relation {rel} を持つこと"
            );
        }
        let accepts_role_member = |rel: &str| {
            thread
                .get("metadata")
                .and_then(|m| m.get("relations"))
                .and_then(|r| r.get(rel))
                .and_then(|r| r.get("directly_related_user_types"))
                .and_then(|v| v.as_array())
                .expect("directly_related_user_types は配列")
                .iter()
                .any(|t| {
                    t.get("type").and_then(|v| v.as_str()) == Some("role")
                        && t.get("relation").and_then(|v| v.as_str()) == Some("member")
                })
        };
        for rel in ["editor", "commenter", "viewer"] {
            assert!(
                accepts_role_member(rel),
                "thread.{rel} は role#member を受理すること（#37）"
            );
        }
    }

    #[test]
    fn fingerprint_extracts_three_keys() {
        // fingerprint は schema_version / type_definitions / conditions の 3 キーのみ持つこと。
        let model = serde_json::json!({
            "id": "01X",
            "schema_version": "1.1",
            "type_definitions": [{"type": "user"}],
            "conditions": {"cond": {}},
            "extra_field": "ignored",
        });
        let fp = model_fingerprint(&model);
        let obj = fp.as_object().expect("fingerprint はオブジェクト");
        assert_eq!(obj.len(), 3);
        assert!(obj.contains_key("schema_version"));
        assert!(obj.contains_key("type_definitions"));
        assert!(obj.contains_key("conditions"));
        // 抽出対象外のフィールドは含まれないこと。
        assert!(!obj.contains_key("id"));
        assert!(!obj.contains_key("extra_field"));
    }

    #[test]
    fn fingerprint_missing_fields_become_null() {
        // 欠落フィールドは Null として埋められること（境界）。
        let model = serde_json::json!({});
        let fp = model_fingerprint(&model);
        assert_eq!(fp.get("schema_version"), Some(&serde_json::Value::Null));
        assert_eq!(fp.get("type_definitions"), Some(&serde_json::Value::Null));
        assert_eq!(fp.get("conditions"), Some(&serde_json::Value::Null));
    }

    #[test]
    fn fingerprint_detects_type_definition_diff() {
        // type_definitions が異なれば fingerprint も異なること（負例: 差分検出）。
        let a = serde_json::json!({
            "schema_version": "1.1",
            "type_definitions": [{"type": "user"}],
        });
        let b = serde_json::json!({
            "schema_version": "1.1",
            "type_definitions": [{"type": "user"}, {"type": "folder"}],
        });
        assert_ne!(model_fingerprint(&a), model_fingerprint(&b));
    }

    #[test]
    fn fingerprint_detects_schema_version_diff() {
        // schema_version が異なれば fingerprint も異なること。
        let a = serde_json::json!({ "schema_version": "1.1" });
        let b = serde_json::json!({ "schema_version": "1.2" });
        assert_ne!(model_fingerprint(&a), model_fingerprint(&b));
    }

    #[test]
    fn fingerprint_detects_conditions_diff() {
        // conditions の差分も検出すること。
        let a = serde_json::json!({ "conditions": {} });
        let b = serde_json::json!({ "conditions": {"c": 1} });
        assert_ne!(model_fingerprint(&a), model_fingerprint(&b));
    }

    #[test]
    fn default_model_fingerprint_is_stable() {
        // 同じ正本 model からは同一 fingerprint が安定して得られること（冪等比較の基盤）。
        let m1 = default_model();
        let m2 = default_model();
        assert_eq!(model_fingerprint(&m1), model_fingerprint(&m2));
    }
}
