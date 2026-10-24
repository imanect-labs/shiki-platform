//! スキーマの改訂と既存データの整合（検証 2-A-P1-1 ①）。
//!
//! 値の無い行がある列への「必須」の付与・行のあるテーブルへの必須の新しい列の追加・使われている選択肢の
//! 削除を通すと、既存の行が検証に通らなくなる。
//! 改訂の Tx（テーブル行を `FOR UPDATE` 済み＝並行する書込はこの前後に直列化される）の中で既存データを
//! 数え、矛盾があれば理由（列の表示名・件数）付きの 400 `schema_conflicts_with_data` で拒否する
//! （一意化の 409 と同じく、改訂の前に数えて止める）。
//!
//! 数えるのは**テーブルの全行**（改訂できるのはスキーマ管理者＝テーブルの owner だけ）。件数は上限付き
//! （[`COUNT_CAP`] 件を超えたら「以上」）。数値の範囲（`numeric_spec`）・型・参照先は改訂で変えられない
//! （[`crate::schema::validate_schema_update`]）ので、ここで見るのは必須と選択肢だけ。

use sqlx::PgConnection;

use crate::field_key::json_key;
use crate::model::{FieldDef, FieldType, TableKeys, TableSchema};
use crate::{map_db, DataError, InvalidCode};

/// 数える件数の上限（これを超えたら「◯件以上」）。
const COUNT_CAP: i64 = 10_000;

/// 改訂で厳しくなる制約ごとに、既存データとの矛盾を数えて拒否する。
pub(crate) async fn check_schema_against_data(
    conn: &mut PgConnection,
    keys: TableKeys,
    current: &TableSchema,
    next: &TableSchema,
) -> Result<(), DataError> {
    // 行のあるテーブルに必須の新しい列を足すと、既存の行がどれも値を持たず検証に通らなくなる（既存の列を
    // 必須にする改訂と同じ矛盾・3 回目の検証 3-A-10）。同じ理由で拒否する。
    for nf in &next.fields {
        if !nf.required
            || nf.field_type.is_derived()
            || nf.is_reverse_link()
            || current.field_by_id(nf.id).is_some()
        {
            continue;
        }
        let n = count(conn, keys, MISSING, &json_key(nf.id), &[]).await?;
        if n > 0 {
            return Err(conflict(
                nf,
                format!(
                    "「{}」は必須の列として追加できません。値の無いレコードが {}あります（必須にしない列として追加し、\
                     値を入れてから必須にしてください）",
                    label(nf),
                    amount(n)
                ),
            ));
        }
    }
    for cur in &current.fields {
        let Some(nf) = next.field_by_id(cur.id) else {
            continue;
        };
        if nf.field_type.is_derived() || nf.is_reverse_link() {
            continue;
        }
        if nf.required && !cur.required {
            let n = count(conn, keys, MISSING, &json_key(nf.id), &[]).await?;
            if n > 0 {
                return Err(conflict(
                    nf,
                    format!(
                        "「{}」に値の無いレコードが {}あるため必須にできません（先に値を入れてください）",
                        label(nf),
                        amount(n)
                    ),
                ));
            }
        }
        if matches!(nf.field_type, FieldType::Select | FieldType::MultiSelect) {
            let removed: Vec<String> = cur
                .options
                .iter()
                .filter(|o| !nf.options.contains(o))
                .cloned()
                .collect();
            if removed.is_empty() {
                continue;
            }
            let n = count(conn, keys, USES_OPTION, &json_key(nf.id), &removed).await?;
            if n > 0 {
                return Err(conflict(
                    nf,
                    format!(
                        "「{}」の選択肢「{}」は {}のレコードで使われているため外せません（先に値を変えてください）",
                        label(nf),
                        removed.join("」「"),
                        amount(n)
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// 値が無い（キーが無い・null・空の配列）。
const MISSING: &str = "(NOT (data ? $3) OR data -> $3 = 'null'::jsonb OR data -> $3 = '[]'::jsonb)";
/// 外す選択肢のどれかを持つ（単一選択は文字列・複数選択は配列の要素）。
const USES_OPTION: &str =
    "((jsonb_typeof(data -> $3) = 'string' AND data ->> $3 = ANY($4::text[])) \
     OR (jsonb_typeof(data -> $3) = 'array' AND data -> $3 ?| $4::text[]))";

async fn count(
    conn: &mut PgConnection,
    keys: TableKeys,
    predicate: &str,
    key: &str,
    options: &[String],
) -> Result<i64, DataError> {
    // 値はすべてバインド（SQL へ埋め込むのは固定の条件文だけ・PIT-21）。`$4` は選択肢（使わない条件でも
    // 型を決めるため `cardinality` で参照する）、`$5` は件数の上限。
    sqlx::query_scalar(&format!(
        "SELECT count(*) FROM (SELECT 1 FROM data_record \
         WHERE tenant_int = $1 AND table_int = $2 AND cardinality($4::text[]) >= 0 AND {predicate} \
         LIMIT $5) s"
    ))
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(key)
    .bind(options)
    .bind(COUNT_CAP + 1)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db)
}

fn label(f: &FieldDef) -> &str {
    f.label()
}

fn amount(n: i64) -> String {
    if n > COUNT_CAP {
        format!("{} 件以上", group_digits(COUNT_CAP))
    } else {
        format!("{} 件", group_digits(n))
    }
}

fn group_digits(n: i64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn conflict(f: &FieldDef, message: String) -> DataError {
    DataError::input_detail(InvalidCode::SchemaConflictsWithData, Some(&f.key), message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_are_capped_and_grouped() {
        assert_eq!(amount(3), "3 件");
        assert_eq!(amount(1234), "1,234 件");
        assert_eq!(amount(COUNT_CAP + 1), "10,000 件以上");
    }
}
