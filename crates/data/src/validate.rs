//! レコード書込のサーバ検証（型・必須・選択肢・参照整合・Task 9.2）。
//!
//! 参照の存在検証は [`RefResolver`] に委譲する（user/role=directory、file=StorageService。
//! record_ref は DB アクセスが要るため [`crate::record`] が担う）。

use async_trait::async_trait;
use authz::AuthContext;
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::model::{FieldDef, FieldType, TableSchema};
use crate::DataError;

/// text フィールドの値の上限長（防御的上限）。
const MAX_TEXT_LEN: usize = 10_000;
/// multi_select の選択数上限。
const MAX_MULTI_SELECT: usize = 50;

/// 参照整合の解決器（user/role/file の存在・可読チェック）。
///
/// api 層が directory / StorageService を束ねて実装する（data crate は
/// ストレージ実体へ依存しない。チョークポイント経由の検証を注入で受ける）。
#[async_trait]
pub trait RefResolver: Send + Sync {
    /// ユーザー（principal id）がテナント内に存在するか。
    async fn user_exists(&self, ctx: &AuthContext, user_id: &str) -> Result<bool, String>;
    /// ロール（部署）がテナント内に存在するか。
    async fn role_exists(&self, ctx: &AuthContext, role_id: &str) -> Result<bool, String>;
    /// ファイルが存在し、**呼出ユーザーが読めるか**（StorageService の authz 込み）。
    async fn file_readable(&self, ctx: &AuthContext, file_id: Uuid) -> Result<bool, String>;
}

/// 書込ペイロードをスキーマで検証し、**正規化済みの保存形**を返す。
///
/// - 未宣言フィールド・派生フィールド（lookup/computed）への書込は拒否。
/// - `full` = create（required の欠落を拒否）。update は merge 後の完全形を渡す。
/// - datetime は UTC 固定幅 ISO-8601 へ正規化する（辞書順＝時刻順）。
pub(crate) async fn validate_record_data(
    ctx: &AuthContext,
    schema: &TableSchema,
    data: &Value,
    resolver: &dyn RefResolver,
) -> Result<Map<String, Value>, DataError> {
    let obj = data.as_object().ok_or_else(|| {
        DataError::Invalid("data は JSON オブジェクトである必要があります".into())
    })?;

    // 未宣言・派生フィールドの拒否（閉じたスキーマ）。
    for key in obj.keys() {
        let Some(f) = schema.field(key) else {
            return Err(DataError::Invalid(format!(
                "フィールド '{key}' はスキーマに存在しません"
            )));
        };
        if matches!(f.field_type, FieldType::Lookup | FieldType::Computed) {
            return Err(DataError::Invalid(format!(
                "フィールド '{key}' は派生フィールドのため書込できません"
            )));
        }
    }

    let mut out = Map::with_capacity(obj.len());
    for f in &schema.fields {
        let value = obj.get(&f.name);
        match value {
            None | Some(Value::Null) => {
                if f.required {
                    return Err(DataError::Invalid(format!(
                        "必須フィールド '{}' がありません",
                        f.name
                    )));
                }
                // 省略・null は「値なし」（保存形にキーを残さない）。
            }
            Some(v) => {
                let normalized = validate_field_value(ctx, f, v, resolver).await?;
                out.insert(f.name.clone(), normalized);
            }
        }
    }
    Ok(out)
}

/// 単一フィールド値の型検証＋正規化。
async fn validate_field_value(
    ctx: &AuthContext,
    f: &FieldDef,
    v: &Value,
    resolver: &dyn RefResolver,
) -> Result<Value, DataError> {
    let name = &f.name;
    let type_err = |want: &str| {
        DataError::Invalid(format!(
            "フィールド '{name}' は {want} である必要があります"
        ))
    };
    match f.field_type {
        FieldType::Text => {
            let s = v.as_str().ok_or_else(|| type_err("文字列"))?;
            if s.len() > MAX_TEXT_LEN {
                return Err(DataError::Invalid(format!(
                    "'{name}' が長すぎます（最大 {MAX_TEXT_LEN} バイト）"
                )));
            }
            Ok(v.clone())
        }
        FieldType::Number => {
            let n = v.as_f64().ok_or_else(|| type_err("数値"))?;
            if !n.is_finite() {
                return Err(type_err("有限の数値"));
            }
            Ok(v.clone())
        }
        FieldType::Date => {
            let s = v.as_str().ok_or_else(|| type_err("YYYY-MM-DD 文字列"))?;
            let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map_err(|_| type_err("YYYY-MM-DD 文字列"))?;
            // chrono はゼロ詰めなし（2026-7-5）も受理するため、**ゼロ詰めの正準形へ正規化**
            // して保存する（辞書順＝日付順と等値フィルタの一致性を保証する）。
            Ok(Value::String(d.format("%Y-%m-%d").to_string()))
        }
        FieldType::DateTime => {
            let s = v.as_str().ok_or_else(|| type_err("RFC3339 文字列"))?;
            let dt = DateTime::parse_from_rfc3339(s).map_err(|_| type_err("RFC3339 文字列"))?;
            // UTC 固定幅（マイクロ秒・Z 終端）へ正規化: 辞書順＝時刻順を保証する。
            let utc: DateTime<Utc> = dt.with_timezone(&Utc);
            Ok(Value::String(
                utc.to_rfc3339_opts(SecondsFormat::Micros, true),
            ))
        }
        FieldType::Select => {
            let s = v.as_str().ok_or_else(|| type_err("文字列"))?;
            if !f.options.iter().any(|o| o == s) {
                return Err(DataError::Invalid(format!(
                    "'{name}' の値 '{s}' は選択肢にありません"
                )));
            }
            Ok(v.clone())
        }
        FieldType::MultiSelect => {
            let arr = v.as_array().ok_or_else(|| type_err("文字列配列"))?;
            if arr.len() > MAX_MULTI_SELECT {
                return Err(DataError::Invalid(format!(
                    "'{name}' の選択数が多すぎます（最大 {MAX_MULTI_SELECT}）"
                )));
            }
            let mut seen = std::collections::HashSet::new();
            for item in arr {
                let s = item.as_str().ok_or_else(|| type_err("文字列配列"))?;
                if !f.options.iter().any(|o| o == s) {
                    return Err(DataError::Invalid(format!(
                        "'{name}' の値 '{s}' は選択肢にありません"
                    )));
                }
                if !seen.insert(s) {
                    return Err(DataError::Invalid(format!(
                        "'{name}' に重複する選択 '{s}' があります"
                    )));
                }
            }
            Ok(v.clone())
        }
        FieldType::UserRef => {
            let s = require_nonempty_str(v, name)?;
            let ok = resolver
                .user_exists(ctx, s)
                .await
                .map_err(DataError::Internal)?;
            if !ok {
                return Err(DataError::Invalid(format!(
                    "'{name}' のユーザー '{s}' が見つかりません"
                )));
            }
            Ok(v.clone())
        }
        FieldType::RoleRef => {
            let s = require_nonempty_str(v, name)?;
            let ok = resolver
                .role_exists(ctx, s)
                .await
                .map_err(DataError::Internal)?;
            if !ok {
                return Err(DataError::Invalid(format!(
                    "'{name}' のロール '{s}' が見つかりません"
                )));
            }
            Ok(v.clone())
        }
        FieldType::FileRef => {
            let id = require_uuid_str(v, name)?;
            let ok = resolver
                .file_readable(ctx, id)
                .await
                .map_err(DataError::Internal)?;
            if !ok {
                // 存在しない/読めないは同一応答（ファイルの存在オラクルを作らない）。
                return Err(DataError::Invalid(format!(
                    "'{name}' のファイルが見つかりません"
                )));
            }
            Ok(v.clone())
        }
        FieldType::RecordRef => {
            // 形式のみここで検証。存在検証は record 層（DB アクセス）が行う。
            require_uuid_str(v, name)?;
            Ok(v.clone())
        }
        FieldType::Lookup | FieldType::Computed => Err(DataError::Invalid(format!(
            "フィールド '{name}' は派生フィールドのため書込できません"
        ))),
    }
}

fn require_nonempty_str<'v>(v: &'v Value, name: &str) -> Result<&'v str, DataError> {
    let s = v
        .as_str()
        .ok_or_else(|| DataError::Invalid(format!("'{name}' は文字列である必要があります")))?;
    if s.is_empty() || s.len() > 256 {
        return Err(DataError::Invalid(format!("'{name}' の id が不正です")));
    }
    Ok(s)
}

fn require_uuid_str(v: &Value, name: &str) -> Result<Uuid, DataError> {
    let s = v.as_str().ok_or_else(|| {
        DataError::Invalid(format!("'{name}' は UUID 文字列である必要があります"))
    })?;
    Uuid::parse_str(s)
        .map_err(|_| DataError::Invalid(format!("'{name}' は UUID 文字列である必要があります")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use authz::{AuthContext, Principal, PrincipalKind};
    use serde_json::json;

    /// 全参照を「存在する」とみなすテスト用リゾルバ。
    struct AllowAll;
    #[async_trait]
    impl RefResolver for AllowAll {
        async fn user_exists(&self, _: &AuthContext, id: &str) -> Result<bool, String> {
            Ok(id != "ghost")
        }
        async fn role_exists(&self, _: &AuthContext, id: &str) -> Result<bool, String> {
            Ok(id != "ghost")
        }
        async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
            Ok(true)
        }
    }

    fn ctx() -> AuthContext {
        AuthContext::new(
            Principal {
                kind: PrincipalKind::User,
                id: "alice".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some("t1".into()),
            },
            "org1".into(),
            "t1".into(),
        )
    }

    fn field(name: &str, ty: FieldType) -> FieldDef {
        FieldDef {
            name: name.into(),
            field_type: ty,
            required: false,
            unique: false,
            indexed: false,
            options: vec![],
            ref_table: None,
            lookup: None,
            computed: None,
        }
    }

    fn schema(fields: Vec<FieldDef>) -> TableSchema {
        TableSchema {
            fields,
            status_field: None,
            row_policy: None,
        }
    }

    #[tokio::test]
    async fn rejects_undeclared_and_derived_fields() {
        let s = schema(vec![field("title", FieldType::Text)]);
        let err = validate_record_data(&ctx(), &s, &json!({"nope": 1}), &AllowAll)
            .await
            .unwrap_err();
        assert!(matches!(err, DataError::Invalid(_)));

        let mut lk = field("d", FieldType::Lookup);
        lk.lookup = Some(crate::model::LookupDef {
            via_field: "r".into(),
            target_field: "x".into(),
        });
        let mut rf = field("r", FieldType::RecordRef);
        rf.ref_table = Some(Uuid::nil());
        let s = schema(vec![rf, lk]);
        let err = validate_record_data(&ctx(), &s, &json!({"d": "v"}), &AllowAll)
            .await
            .unwrap_err();
        assert!(matches!(err, DataError::Invalid(_)));
    }

    #[tokio::test]
    async fn required_and_types_enforced() {
        let mut t = field("title", FieldType::Text);
        t.required = true;
        let s = schema(vec![t, field("count", FieldType::Number)]);
        // 必須欠落。
        assert!(validate_record_data(&ctx(), &s, &json!({}), &AllowAll)
            .await
            .is_err());
        // 型不一致。
        assert!(
            validate_record_data(&ctx(), &s, &json!({"title": "a", "count": "x"}), &AllowAll)
                .await
                .is_err()
        );
        // 正常＋null は「値なし」。
        let out =
            validate_record_data(&ctx(), &s, &json!({"title": "a", "count": null}), &AllowAll)
                .await
                .unwrap();
        assert_eq!(out.get("title"), Some(&json!("a")));
        assert!(!out.contains_key("count"));
    }

    #[tokio::test]
    async fn datetime_normalized_to_utc_fixed_width() {
        let s = schema(vec![field("at", FieldType::DateTime)]);
        let out = validate_record_data(
            &ctx(),
            &s,
            &json!({"at": "2026-07-10T09:00:00+09:00"}),
            &AllowAll,
        )
        .await
        .unwrap();
        assert_eq!(out.get("at"), Some(&json!("2026-07-10T00:00:00.000000Z")));
        // 不正形式は拒否。
        assert!(
            validate_record_data(&ctx(), &s, &json!({"at": "2026/07/10"}), &AllowAll)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn select_and_multi_select_closed_options() {
        let mut sel = field("st", FieldType::Select);
        sel.options = vec!["open".into(), "closed".into()];
        let mut ms = field("tags", FieldType::MultiSelect);
        ms.options = vec!["a".into(), "b".into()];
        let s = schema(vec![sel, ms]);
        assert!(
            validate_record_data(&ctx(), &s, &json!({"st": "open", "tags": ["a"]}), &AllowAll)
                .await
                .is_ok()
        );
        assert!(
            validate_record_data(&ctx(), &s, &json!({"st": "unknown"}), &AllowAll)
                .await
                .is_err()
        );
        assert!(
            validate_record_data(&ctx(), &s, &json!({"tags": ["a", "a"]}), &AllowAll)
                .await
                .is_err()
        );
        assert!(
            validate_record_data(&ctx(), &s, &json!({"tags": ["z"]}), &AllowAll)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn user_ref_checked_via_resolver() {
        let s = schema(vec![field("assignee", FieldType::UserRef)]);
        assert!(
            validate_record_data(&ctx(), &s, &json!({"assignee": "bob"}), &AllowAll)
                .await
                .is_ok()
        );
        assert!(
            validate_record_data(&ctx(), &s, &json!({"assignee": "ghost"}), &AllowAll)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn record_ref_requires_uuid_format() {
        let mut rf = field("parent", FieldType::RecordRef);
        rf.ref_table = Some(Uuid::nil());
        let s = schema(vec![rf]);
        assert!(
            validate_record_data(&ctx(), &s, &json!({"parent": "not-a-uuid"}), &AllowAll)
                .await
                .is_err()
        );
        assert!(validate_record_data(
            &ctx(),
            &s,
            &json!({"parent": Uuid::nil().to_string()}),
            &AllowAll
        )
        .await
        .is_ok());
    }
}
