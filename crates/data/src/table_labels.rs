//! 見えるテーブルの名前をまとめて引く（GAP-37・アプリとワークフローを使う人の権限の確かめ）。
//!
//! 共有の画面・公開の確認・アプリの画面が、部品の書き先のテーブルを名前で言うために使う。名前を返すのは呼んだ人に
//! 見える（閲覧か追加だけ）テーブルだけで、見えない・無い・消したテーブルは返さない（存在を区別させない）。
//!
//! 二段で絞る: DB の候補（テナント・組織・生きているもの・`id` で選んだもの）を先に引き（pre-filter）、OpenFGA の
//! BatchCheck（テーブルのメモ・[`crate::policy::memo::AuthzMemo`]）で絞る（post-filter）。定義もレコードも読まないので
//! 監査には残さない（テーブルの一覧と同じ扱い）。

use std::collections::HashMap;

use authz::{AuthContext, Relation};
use uuid::Uuid;

use crate::store::DataStore;
use crate::{map_db, DataError};

/// 1 回に名前を引けるテーブルの数の上限（呼び出し側は上限までに切る）。
pub const MAX_TABLE_LABELS: usize = 200;

/// テーブルの名前と持ち主のアプリ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableLabel {
    pub name: String,
    /// インストールしたアプリが持つテーブルなら、そのアプリ（テーブルの共有ではなく、アプリの「使える部署・グループ」で
    /// 渡す・GAP-68）。
    pub app_id: Option<Uuid>,
}

impl DataStore {
    /// `ids` のうち、呼んだ人に見える（閲覧か追加だけ）テーブルの名前。見えない・無い・消したものは入れない。
    ///
    /// `ids` が [`MAX_TABLE_LABELS`] を超えたら誤り（呼び出し側の上限の誤り・問う数を抑える）。
    pub async fn table_labels(
        &self,
        ctx: &AuthContext,
        ids: &[Uuid],
    ) -> Result<HashMap<Uuid, TableLabel>, DataError> {
        if ids.len() > MAX_TABLE_LABELS {
            return Err(DataError::Internal(format!(
                "テーブルの名前は 1 回に {MAX_TABLE_LABELS} 件までしか引けません"
            )));
        }
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows: Vec<(Uuid, String, Option<Uuid>)> = sqlx::query_as(
            "SELECT id, name, app_id FROM data_table \
             WHERE tenant_id = $1 AND org = $2 AND id = ANY($3) AND deleted_at IS NULL",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(ids)
        .fetch_all(self.pools.for_tenant(ctx.tenant_id()))
        .await
        .map_err(map_db)?;
        if rows.is_empty() {
            return Ok(HashMap::new());
        }
        let items: Vec<(Relation, authz::FgaObject)> = rows
            .iter()
            .flat_map(|(id, _, _)| {
                let obj = ctx.ns().data_table(&id.to_string());
                [(Relation::Viewer, obj.clone()), (Relation::Submitter, obj)]
            })
            .collect();
        let held = self.authz_memo().check_all(ctx, &items).await?;
        Ok(rows
            .into_iter()
            .zip(held.chunks(2))
            .filter(|(_, h)| h.iter().any(|b| *b))
            .map(|((id, name, app_id), _)| (id, TableLabel { name, app_id }))
            .collect())
    }
}
