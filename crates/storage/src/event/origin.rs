//! 書込の由来の run（run の書込の印・BUG-44）。
//!
//! ワークフローの run の中の書込は、文脈（[`AuthContext::origin_run`]）に run を持つ。発行
//! （[`super::emit_on`] / [`super::emit_many_on`]）はそれを outbox の consumer 非公開部（`private_payload`）の
//! [`ORIGIN_RUN_KEY`] に**必ず**載せる（run の書込でなければ `null`）。汎用の consumer は非公開部を読まないので、
//! 印はワークフローの relay だけが [`write_origins`] で読む（因果の鎖・`by_workflow`）。
//!
//! 印は利用者が名乗れない場所にある: 利用者の要求から組む文脈は `origin_run` を持たず、要求が運ぶ値
//! （trace_id など）から印は決まらない。印のキーが無い行（[`WriteOrigin::Unmarked`]）は、この印を書く前の版が
//! 発行した行だけ（移行の間・ローリングデプロイの古い版）で、読む側が従来の扱いに倒す。
//!
//! [`AuthContext::origin_run`]: authz::AuthContext::origin_run

use std::collections::HashMap;

use authz::AuthContext;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::error::StorageError;

/// 非公開部の、書込の由来の run のキー（値は run id か `null`）。
pub const ORIGIN_RUN_KEY: &str = "origin_run";

/// outbox の 1 件の書込の由来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOrigin {
    /// run の中の書込（その run）。
    Run(Uuid),
    /// run の書込ではない（利用者の操作・system の保守など）。
    NotRun,
    /// 印の無い行（印を書く前の版が発行した行）。
    Unmarked,
}

/// 非公開部に載せる印の値（文脈の run か `null`）。
pub(super) fn origin_value(ctx: &AuthContext) -> Value {
    ctx.origin_run()
        .map_or(Value::Null, |r| Value::String(r.to_string()))
}

/// 非公開部に印を足す（非公開部が無ければ印だけの非公開部を作る）。
pub(super) fn with_origin(ctx: &AuthContext, private: Option<&Value>) -> Value {
    let mut out = match private {
        Some(Value::Object(m)) => Value::Object(m.clone()),
        _ => Value::Object(serde_json::Map::new()),
    };
    if let Value::Object(m) = &mut out {
        m.insert(ORIGIN_RUN_KEY.into(), origin_value(ctx));
    }
    out
}

/// 行の非公開部（の印のキーの有無と値）から由来を読む。
fn origin_of(marked: Option<bool>, run: Option<&str>) -> WriteOrigin {
    match (marked, run) {
        (Some(true), Some(r)) => Uuid::parse_str(r).map_or(WriteOrigin::NotRun, WriteOrigin::Run),
        (Some(true), None) => WriteOrigin::NotRun,
        _ => WriteOrigin::Unmarked,
    }
}

/// outbox の id → 書込の由来（[`ORIGIN_RUN_KEY`] だけを読む・ほかの非公開の材料は外へ出さない）。
///
/// 見つからない id（GC 済み）は含めない。
pub async fn write_origins(
    conn: &mut PgConnection,
    ids: &[i64],
) -> Result<HashMap<i64, WriteOrigin>, StorageError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    // tenant-sql: allow(配送したイベントの outbox の id（大域一意の内部の id）で由来の印だけを引く)
    let rows: Vec<(i64, Option<bool>, Option<String>)> = sqlx::query_as(
        "SELECT id, private_payload ? $2, private_payload ->> $2 \
         FROM storage_event_outbox WHERE id = ANY($1)",
    )
    .bind(ids)
    .bind(ORIGIN_RUN_KEY)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, marked, run)| (id, origin_of(marked, run.as_deref())))
        .collect())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use authz::{Principal, PrincipalKind};
    use serde_json::json;

    fn ctx() -> AuthContext {
        AuthContext::new(
            Principal {
                kind: PrincipalKind::User,
                id: "alice".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some("t".into()),
            },
            "o".into(),
            authz::TenantId::parse("t").unwrap(),
        )
    }

    /// 印はいつも載る（run の書込でなければ null）。既存の非公開部の材料は残す。
    #[test]
    fn the_mark_is_always_written() {
        let run = Uuid::new_v4();
        assert_eq!(with_origin(&ctx(), None), json!({ "origin_run": null }));
        assert_eq!(
            with_origin(&ctx().in_run(Some(run)), Some(&json!({ "bulk": true }))),
            json!({ "bulk": true, "origin_run": run.to_string() })
        );
    }

    /// キーがあれば印を信じ、キーが無い行（印を書く前の版の行）だけを区別する。
    #[test]
    fn rows_without_the_key_are_unmarked() {
        let run = Uuid::new_v4();
        assert_eq!(
            origin_of(Some(true), Some(&run.to_string())),
            WriteOrigin::Run(run)
        );
        assert_eq!(origin_of(Some(true), None), WriteOrigin::NotRun);
        assert_eq!(origin_of(Some(false), None), WriteOrigin::Unmarked);
        assert_eq!(origin_of(None, None), WriteOrigin::Unmarked);
    }
}
