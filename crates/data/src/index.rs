//! 宣言フィールドへの式インデックス管理（Task 9.2）。
//!
//! スキーマ登録/改訂時（管理操作）に、`indexed` / `unique` 宣言されたフィールドへ
//! JSONB 式インデックスを冪等に適用する。**ランタイム DDL は打たない**（レコード
//! 書込経路に DDL は存在しない）。
//!
//! # SQL 埋め込みの安全性
//!
//! インデックス DDL はプレースホルダを使えないため、式にフィールド名・述語に
//! tenant_id / table_id を埋め込む。安全性は次で担保する:
//! - フィールド名: スキーマ検証（`^[a-z][a-z0-9_]{0,63}$`・[`crate::schema`]）を通過した値のみ。
//!   さらに本モジュールでも埋め込み直前に再検証する（防御の二重化）。
//! - table_id: `uuid::Uuid` の Display（hex とハイフンのみ）。
//! - tenant_id: 単一引用符を含む値を拒否する（正規のテナント id は英数字）。

use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::model::{FieldDef, FieldType, TableSchema};
use crate::schema::is_valid_field_name;
use crate::{map_db, DataError};

/// インデックス種別（台帳 `data_index_registry.kind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndexKind {
    /// text 系（text/select/date/datetime/user_ref/role_ref/file_ref/record_ref）の btree。
    BtreeText,
    /// number の numeric キャスト btree。
    BtreeNumeric,
    /// multi_select の GIN（`?` 存在演算子）。
    Gin,
    /// unique 宣言フィールドの一意 btree（型別式は Btree* と同じ）。
    UniqueText,
    UniqueNumeric,
}

impl IndexKind {
    fn as_str(self) -> &'static str {
        match self {
            IndexKind::BtreeText => "btree_text",
            IndexKind::BtreeNumeric => "btree_numeric",
            IndexKind::Gin => "gin",
            IndexKind::UniqueText => "unique_text",
            IndexKind::UniqueNumeric => "unique_numeric",
        }
    }
}

/// フィールド定義から必要なインデックス（0〜2 個: 検索用＋unique 用）を導く。
fn plan_for_field(f: &FieldDef) -> Vec<IndexKind> {
    let mut plan = Vec::new();
    if f.unique {
        // unique インデックスは検索にも使えるため、indexed 併用時も 1 本で足りる。
        plan.push(match f.field_type {
            FieldType::Number => IndexKind::UniqueNumeric,
            _ => IndexKind::UniqueText,
        });
        return plan;
    }
    if f.indexed {
        plan.push(match f.field_type {
            FieldType::Number => IndexKind::BtreeNumeric,
            FieldType::MultiSelect => IndexKind::Gin,
            _ => IndexKind::BtreeText,
        });
    }
    plan
}

/// 決定的なインデックス名 `dr_<table 先頭8hex>_<sha256(field:kind) 先頭8hex>`。
///
/// PostgreSQL の識別子上限（63 バイト）に収まり、テーブル×フィールド×種別で一意。
fn index_name(table_id: Uuid, field: &str, kind: IndexKind) -> String {
    let tid = table_id.simple().to_string();
    let mut hasher = Sha256::new();
    hasher.update(field.as_bytes());
    hasher.update(b":");
    hasher.update(kind.as_str().as_bytes());
    let digest = hex_prefix(&hasher.finalize(), 8);
    format!("dr_{}_{digest}", &tid[..8])
}

fn hex_prefix(bytes: &[u8], len: usize) -> String {
    let mut s = String::with_capacity(len);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        if s.len() >= len {
            s.truncate(len);
            break;
        }
    }
    s
}

/// tenant_id を DDL 述語へ埋め込んでよいか検証する（単一引用符を拒否）。
fn safe_tenant(tenant_id: &str) -> Result<&str, DataError> {
    if tenant_id.contains('\'') || tenant_id.is_empty() {
        return Err(DataError::Internal(
            "tenant_id が DDL に埋め込めません".into(),
        ));
    }
    Ok(tenant_id)
}

/// インデックス式（フィールド名は検証済み前提＋直前再検証）。
fn index_expression(field: &str, kind: IndexKind) -> Result<String, DataError> {
    if !is_valid_field_name(field) {
        return Err(DataError::Internal(format!(
            "フィールド名 '{field}' が識別子規約外です"
        )));
    }
    Ok(match kind {
        IndexKind::BtreeText | IndexKind::UniqueText => format!("((data ->> '{field}'))"),
        IndexKind::BtreeNumeric | IndexKind::UniqueNumeric => {
            format!("(((data ->> '{field}'))::numeric)")
        }
        IndexKind::Gin => format!("((data -> '{field}'))"),
    })
}

/// 1 本分の CREATE INDEX DDL。
fn create_ddl(
    name: &str,
    tenant_id: &str,
    table_id: Uuid,
    field: &str,
    kind: IndexKind,
) -> Result<String, DataError> {
    let expr = index_expression(field, kind)?;
    let tenant = safe_tenant(tenant_id)?;
    let unique = matches!(kind, IndexKind::UniqueText | IndexKind::UniqueNumeric);
    let method = if kind == IndexKind::Gin {
        " USING gin"
    } else {
        ""
    };
    Ok(format!(
        "CREATE {unique_kw}INDEX IF NOT EXISTS {name} ON data_record{method} ({expr}) \
         WHERE tenant_id = '{tenant}' AND table_id = '{table_id}'",
        unique_kw = if unique { "UNIQUE " } else { "" },
    ))
}

/// 台帳 1 行。
#[derive(sqlx::FromRow)]
struct RegistryRow {
    field: String,
    index_name: String,
    kind: String,
}

/// スキーマ宣言と台帳を突き合わせ、不足分の作成・不要分の削除を冪等に適用する。
///
/// テーブル作成/スキーマ改訂と同一トランザクション内で呼ぶ（`CREATE INDEX` は
/// トランザクション内で実行可能。作成直後のテーブルは空なので即時に完了する）。
pub(crate) async fn ensure_indexes(
    conn: &mut PgConnection,
    tenant_id: &str,
    table_id: Uuid,
    schema: &TableSchema,
) -> Result<(), DataError> {
    // 望ましい状態: (field, kind, index_name)。
    let mut desired: Vec<(String, IndexKind, String)> = Vec::new();
    for f in &schema.fields {
        for kind in plan_for_field(f) {
            desired.push((f.name.clone(), kind, index_name(table_id, &f.name, kind)));
        }
    }

    let existing: Vec<RegistryRow> = sqlx::query_as(
        "SELECT field, index_name, kind FROM data_index_registry \
         WHERE tenant_id = $1 AND table_id = $2",
    )
    .bind(tenant_id)
    .bind(table_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db)?;

    // 不要になったインデックス（宣言から外れた・種別が変わった）を落とす。
    for row in &existing {
        let still_wanted = desired
            .iter()
            .any(|(_, kind, name)| *name == row.index_name && kind.as_str() == row.kind);
        if !still_wanted {
            // index_name は台帳経由（過去に本モジュールが決定的命名で作ったもの）のみ。
            sqlx::query(&format!("DROP INDEX IF EXISTS {}", row.index_name))
                .execute(&mut *conn)
                .await
                .map_err(map_db)?;
            sqlx::query(
                "DELETE FROM data_index_registry \
                 WHERE tenant_id = $1 AND table_id = $2 AND field = $3",
            )
            .bind(tenant_id)
            .bind(table_id)
            .bind(&row.field)
            .execute(&mut *conn)
            .await
            .map_err(map_db)?;
        }
    }

    // 不足分を作成し台帳へ記録する。
    for (field, kind, name) in &desired {
        let already = existing
            .iter()
            .any(|row| row.index_name == *name && row.kind == kind.as_str());
        if already {
            continue;
        }
        let ddl = create_ddl(name, tenant_id, table_id, field, *kind)?;
        sqlx::query(&ddl).execute(&mut *conn).await.map_err(|e| {
            // unique インデックス作成は既存データの重複で失敗し得る → 409 として返す。
            if matches!(kind, IndexKind::UniqueText | IndexKind::UniqueNumeric) {
                DataError::Conflict(format!(
                    "フィールド '{field}' に重複値があるため unique を適用できません: {e}"
                ))
            } else {
                map_db(e)
            }
        })?;
        sqlx::query(
            "INSERT INTO data_index_registry (tenant_id, table_id, field, index_name, kind) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (tenant_id, table_id, field) \
             DO UPDATE SET index_name = excluded.index_name, kind = excluded.kind",
        )
        .bind(tenant_id)
        .bind(table_id)
        .bind(field)
        .bind(name)
        .bind(kind.as_str())
        .execute(&mut *conn)
        .await
        .map_err(map_db)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, ty: FieldType, indexed: bool, unique: bool) -> FieldDef {
        FieldDef {
            name: name.into(),
            field_type: ty,
            required: false,
            unique,
            indexed,
            options: vec![],
            ref_table: None,
            lookup: None,
            computed: None,
        }
    }

    #[test]
    fn plan_maps_types_to_kinds() {
        assert_eq!(
            plan_for_field(&field("t", FieldType::Text, true, false)),
            vec![IndexKind::BtreeText]
        );
        assert_eq!(
            plan_for_field(&field("n", FieldType::Number, true, false)),
            vec![IndexKind::BtreeNumeric]
        );
        assert_eq!(
            plan_for_field(&field("m", FieldType::MultiSelect, true, false)),
            vec![IndexKind::Gin]
        );
        // unique は 1 本に集約（indexed 併用でも unique インデックスが検索を兼ねる）。
        assert_eq!(
            plan_for_field(&field("u", FieldType::Text, true, true)),
            vec![IndexKind::UniqueText]
        );
        assert!(plan_for_field(&field("x", FieldType::Text, false, false)).is_empty());
    }

    #[test]
    fn index_name_is_deterministic_and_short() {
        let tid = Uuid::parse_str("6f1b24a0-0000-0000-0000-000000000000").unwrap();
        let a = index_name(tid, "title", IndexKind::BtreeText);
        let b = index_name(tid, "title", IndexKind::BtreeText);
        assert_eq!(a, b);
        assert!(a.starts_with("dr_6f1b24a0_"));
        assert!(a.len() <= 63);
        // 種別が違えば名前も違う（型変更時に別インデックスとして張り替わる）。
        assert_ne!(a, index_name(tid, "title", IndexKind::UniqueText));
    }

    #[test]
    fn ddl_embeds_only_validated_parts() {
        let tid = Uuid::nil();
        let ddl = create_ddl("dr_x_y", "acme", tid, "price", IndexKind::UniqueNumeric).unwrap();
        assert!(ddl.contains("CREATE UNIQUE INDEX IF NOT EXISTS dr_x_y"));
        assert!(ddl.contains("(((data ->> 'price'))::numeric)"));
        assert!(ddl.contains("tenant_id = 'acme'"));
        // 不正なフィールド名・tenant は拒否（防御の二重化）。
        assert!(create_ddl("i", "acme", tid, "p'; drop", IndexKind::BtreeText).is_err());
        assert!(create_ddl("i", "ac'me", tid, "price", IndexKind::BtreeText).is_err());
    }

    #[test]
    fn gin_ddl_uses_gin_method() {
        let ddl = create_ddl("i", "acme", Uuid::nil(), "tags", IndexKind::Gin).unwrap();
        assert!(ddl.contains("USING gin"));
        assert!(ddl.contains("((data -> 'tags'))"));
    }
}
