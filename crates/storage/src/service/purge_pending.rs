//! StorageService: 完全削除の片付け待ち（`storage_purge_pending`・GAP-108）の掃除。
//!
//! 完全削除は、ノード・版・blob の行を 1 つのトランザクションで消し、DB の外にあるもの（OpenFGA のタプルと、
//! オブジェクトストアの本体）を消す仕事を同じトランザクションで `storage_purge_pending` に積む（migration 0152）。
//! ここは commit の後にそれを片付ける。中断アップロードの回収タイマ（`upload_gc_timer`）の回に相乗りする。
//!
//! - `node_tuples`: そのノードに直接張られたタプルをすべて外す（[`AuthzClient::delete_object_tuples`]・冪等）。
//! - `object`: blob のロック（[`lock_blob`]・`bump_blob` と同じキー）の中で、その blob の行が無いことを確かめて
//!   から本体を消す。同じ内容が同時に保存されて行が作り直されていれば消さない（共有の本体を消して他の参照を
//!   壊さない・PIT-14）。行を作り直した書き込みは、同じロックの中で本体を確かめ直す（`bump_blob`）ので、掃除が
//!   先に消していても最後には本体が残る。
//!
//! 1 行ごとにテナントを横断するトランザクション（[`begin_cross_tenant_tx`]）を開き、古い行から
//! `FOR UPDATE SKIP LOCKED` で取る（複数のレプリカが同じ行を奪い合わない）。成功した行は消す。失敗した行は
//! `attempts` と `last_error` を残して次の回に回す（同じ回の中では 1 行を 1 度しか試さない）。

#[allow(clippy::wildcard_imports)]
use super::*;

use sqlx::Connection as _;

use super::upload::lock_blob;
use crate::db_tenant::begin_cross_tenant_tx;

/// テナントを横断するトランザクションの目的（`application_name` に載る）。
const PURPOSE: &str = "storage-purge-pending";

/// 1 回の掃除で試す行の上限（1 行 = 1 トランザクション ＋ 外への呼び出し 1 回）。残りは次の回に回す。
const MAX_ROWS_PER_CYCLE: usize = 2000;

/// `last_error` に残す長さの上限（文字数）。
const MAX_ERROR_CHARS: usize = 1000;

/// 片付け待ちの 1 行。
#[derive(sqlx::FromRow)]
struct PurgeRow {
    tenant_id: String,
    id: Uuid,
    org: String,
    kind: String,
    node_id: Option<Uuid>,
    node_kind: Option<String>,
    sha256: Option<String>,
    object_key: Option<String>,
}

/// [`StorageService::sweep_purge_pending`] の結果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PurgeSweep {
    /// 片付けて消した行の数（本体の行が作り直されていて、本体を消さずに終えた行も含む）。
    pub done: usize,
    /// 失敗して `attempts` を進めた行の数（次の回に回す）。
    pub failed: usize,
}

impl StorageService {
    /// 完全削除の片付け待ちを古い行から片付ける（テナント横断の保守処理）。
    ///
    /// 行ごとの失敗（OpenFGA・オブジェクトストアの誤り・行の形の不正）は `attempts` と `last_error` に残し、
    /// [`PurgeSweep::failed`] に数えて続ける。DB に届かない（行を取れない・失敗を書けない）ときだけ `Err` を返す。
    pub async fn sweep_purge_pending(&self) -> Result<PurgeSweep, StorageError> {
        let mut report = PurgeSweep::default();
        let mut seen: Vec<Uuid> = Vec::new();
        while seen.len() < MAX_ROWS_PER_CYCLE {
            let mut tx = begin_cross_tenant_tx(&self.db, PURPOSE).await?;
            // tenant-sql: allow(テナントを横断する保守の掃除・begin_cross_tenant_tx で RLS を迂回する)
            let row: Option<PurgeRow> = sqlx::query_as(
                "SELECT tenant_id, id, org, kind, node_id, node_kind, sha256, object_key \
                 FROM storage_purge_pending \
                 WHERE NOT (id = ANY($1)) \
                 ORDER BY created_at LIMIT 1 FOR UPDATE SKIP LOCKED",
            )
            .bind(&seen)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(row) = row else {
                tx.rollback().await?;
                break;
            };
            seen.push(row.id);

            // 片付けは savepoint の中で行う。DB の誤りで Tx が壊れても、失敗を同じ Tx に書ける。
            // blob のロックは savepoint を解放しても外側の Tx の終わりまで残る（本体の削除と行の削除が同じロックの中）。
            let outcome = {
                let mut sp = tx.begin().await?;
                match self.purge_one(&mut sp, &row).await {
                    Ok(()) => {
                        sp.commit().await?;
                        Ok(())
                    }
                    Err(e) => {
                        sp.rollback().await?;
                        Err(e)
                    }
                }
            };
            match outcome {
                Ok(()) => {
                    sqlx::query(
                        "DELETE FROM storage_purge_pending WHERE tenant_id = $1 AND id = $2",
                    )
                    .bind(&row.tenant_id)
                    .bind(row.id)
                    .execute(&mut *tx)
                    .await?;
                    report.done += 1;
                }
                Err(e) => {
                    let message: String = e.to_string().chars().take(MAX_ERROR_CHARS).collect();
                    tracing::warn!(
                        error = %message,
                        tenant_id = %row.tenant_id,
                        id = %row.id,
                        kind = %row.kind,
                        "完全削除の片付けに失敗しました（次の回に回します）"
                    );
                    sqlx::query(
                        "UPDATE storage_purge_pending \
                         SET attempts = attempts + 1, last_error = $3 \
                         WHERE tenant_id = $1 AND id = $2",
                    )
                    .bind(&row.tenant_id)
                    .bind(row.id)
                    .bind(&message)
                    .execute(&mut *tx)
                    .await?;
                    report.failed += 1;
                }
            }
            tx.commit().await?;
        }
        Ok(report)
    }

    /// 1 行を片付ける（`conn` は掃除の Tx の savepoint）。
    async fn purge_one(&self, conn: &mut PgConnection, row: &PurgeRow) -> Result<(), StorageError> {
        // 片付けの名前空間（FGA の識別子・オブジェクトキー）は tenant_id から組む。文字ポリシーに反する値では
        // 組まない（テナント境界の外のものを消さない・fail-closed）。
        let tenant = TenantId::parse(row.tenant_id.as_str())
            .map_err(|e| StorageError::Invalid(format!("tenant_id が不正です: {e}")))?;
        match row.kind.as_str() {
            "node_tuples" => {
                let (Some(node_id), Some(kind)) = (row.node_id, row.node_kind.as_deref()) else {
                    return Err(StorageError::Invalid(
                        "node_tuples の行に node_id・node_kind がありません".into(),
                    ));
                };
                let kind = NodeKind::parse(kind)
                    .ok_or_else(|| StorageError::Invalid(format!("未知のノード種別: {kind}")))?;
                let object =
                    node_fga_object(&Namespace::for_tenant(tenant.as_str()), kind, node_id);
                let removed = self.authz.delete_object_tuples(&object).await?;
                tracing::info!(
                    tenant_id = %row.tenant_id,
                    node_id = %node_id,
                    removed,
                    "完全削除したノードのタプルを外しました"
                );
                Ok(())
            }
            "object" => {
                let (Some(sha256), Some(key)) = (row.sha256.as_deref(), row.object_key.as_deref())
                else {
                    return Err(StorageError::Invalid(
                        "object の行に sha256・object_key がありません".into(),
                    ));
                };
                // 消すのは、その (tenant_id, org, sha256) の content-addressed のキーだけ（別のキーを消さない）。
                if !is_valid_sha256_hex(sha256)
                    || key != blob_object_key(tenant.as_str(), &row.org, sha256)
                {
                    return Err(StorageError::Invalid(format!(
                        "object_key が blob のキーと一致しません（{key}）"
                    )));
                }
                lock_blob(&mut *conn, tenant.as_str(), &row.org, sha256).await?;
                let live: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM blob WHERE tenant_id = $1 AND org = $2 AND sha256 = $3)",
                )
                .bind(tenant.as_str())
                .bind(&row.org)
                .bind(sha256)
                .fetch_one(&mut *conn)
                .await?;
                if live {
                    // 同じ内容が保存し直されて行がある（本体はその参照のもの）。消さずに終える。
                    tracing::info!(
                        tenant_id = %row.tenant_id,
                        sha256,
                        "blob の行が作り直されていたので、本体を消さずに片付けを終えました"
                    );
                    return Ok(());
                }
                self.store.delete(key).await?;
                tracing::info!(
                    tenant_id = %row.tenant_id,
                    sha256,
                    "完全削除した blob の本体を消しました"
                );
                Ok(())
            }
            other => Err(StorageError::Invalid(format!(
                "未知の片付けの種類: {other}"
            ))),
        }
    }
}
