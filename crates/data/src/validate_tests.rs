//! data レコード検証（validate_record_data / validate_field_value）の検証マトリクス。
//! validate.rs の 500 行上限を守るため #[path] で分離する（純粋・fake resolver）。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic
)]

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
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
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
    let out = validate_record_data(&ctx(), &s, &json!({"title": "a", "count": null}), &AllowAll)
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

/// file_ref を不可読にするリゾルバ（file_readable=false）。
struct DenyFiles;
#[async_trait]
impl RefResolver for DenyFiles {
    async fn user_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    async fn role_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
        Ok(false)
    }
}

#[tokio::test]
async fn text_length_and_number_type_limits() {
    let s = schema(vec![
        field("t", FieldType::Text),
        field("n", FieldType::Number),
    ]);
    // text 上限超過。
    let long = "x".repeat(10_001);
    assert!(
        validate_record_data(&ctx(), &s, &json!({ "t": long }), &AllowAll)
            .await
            .is_err()
    );
    // number に文字列は不可・数値は可。
    assert!(
        validate_record_data(&ctx(), &s, &json!({ "n": "5" }), &AllowAll)
            .await
            .is_err()
    );
    assert!(
        validate_record_data(&ctx(), &s, &json!({ "n": 5 }), &AllowAll)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn date_normalizes_and_rejects_bad_format() {
    let s = schema(vec![field("d", FieldType::Date)]);
    // ゼロ詰めなしを正準形へ正規化。
    let out = validate_record_data(&ctx(), &s, &json!({ "d": "2026-7-5" }), &AllowAll)
        .await
        .unwrap();
    assert_eq!(out.get("d"), Some(&json!("2026-07-05")));
    // 非日付・非文字列は拒否。
    assert!(
        validate_record_data(&ctx(), &s, &json!({ "d": "07/05/2026" }), &AllowAll)
            .await
            .is_err()
    );
    assert!(
        validate_record_data(&ctx(), &s, &json!({ "d": 20260705 }), &AllowAll)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn role_ref_and_file_ref_checked_via_resolver() {
    let role = schema(vec![field("dept", FieldType::RoleRef)]);
    assert!(
        validate_record_data(&ctx(), &role, &json!({ "dept": "sales" }), &AllowAll)
            .await
            .is_ok()
    );
    assert!(
        validate_record_data(&ctx(), &role, &json!({ "dept": "ghost" }), &AllowAll)
            .await
            .is_err()
    );
    // file_ref: 読めないファイルは拒否（存在秘匿の fail-closed）。
    let file = schema(vec![field("att", FieldType::FileRef)]);
    assert!(validate_record_data(
        &ctx(),
        &file,
        &json!({ "att": Uuid::nil().to_string() }),
        &DenyFiles
    )
    .await
    .is_err());
}

#[tokio::test]
async fn multi_select_shape_and_count_limits() {
    let mut ms = field("tags", FieldType::MultiSelect);
    ms.options = (0..60).map(|i| format!("o{i}")).collect();
    let s = schema(vec![ms]);
    // 配列でない。
    assert!(
        validate_record_data(&ctx(), &s, &json!({ "tags": "o1" }), &AllowAll)
            .await
            .is_err()
    );
    // 選択数上限（50）超過。
    let too_many: Vec<String> = (0..51).map(|i| format!("o{i}")).collect();
    assert!(
        validate_record_data(&ctx(), &s, &json!({ "tags": too_many }), &AllowAll)
            .await
            .is_err()
    );
}
