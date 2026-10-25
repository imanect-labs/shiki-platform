//! 購読者の認可材料（SSE 購読中に限る TTL 付きの再解決・data-platform §9.4・PIT-70）。
//!
//! - 購読の開始時（[`crate::DataStore::subscribe_records`]）に、テーブル viewer・行述語の材料
//!   （ロール・名指しロール・個別共有集合）・フィールドマスクを OpenFGA から引く。
//! - 以後は **TTL（既定 5 秒）の間だけ**その答えを持ち越す（[`crate::policy::memo::MemoCarry`]）。
//!   TTL を過ぎた購読は配信の**前に**引き直す＝剥奪の配信への反映は最大 TTL 遅れる（製品の約束）。
//! - ロール変更・共有解除・テーブル権限の変更（世代 [`super::invalidate`]）・スキーマ改訂を検知した
//!   購読は TTL を待たずに引き直す。
//! - 引き直した結果が前回と違えば（viewer の喪失・ロール・共有・マスクの変化）、行ごとの差分では
//!   取りこぼしが出る（権限側の変化で見えなくなった行にはイベントが無い）ので、**購読ごと落とす**
//!   （`stream.reset` を送って閉じる・クライアントは取り直す・PIT-73 ③）。
//! - REST の読取経路はこれを使わない（毎回解決）。

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use authz::{AuthContext, Relation};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::model::DataTable;
use crate::policy::material::{self, PolicyMaterial};
use crate::policy::memo::{AuthzMemo, MemoCarry};
use crate::store::DataStore;
use crate::DataError;

/// 購読者 1 人の材料（TTL の間だけ持ち越す）。
pub(crate) struct SubAuth {
    pub carry: Option<MemoCarry>,
    pub resolved_at: Instant,
    pub schema_version: i64,
    pub material: Arc<PolicyMaterial>,
    pub masked: HashSet<String>,
    pub shared: HashSet<Uuid>,
    /// 材料の指紋（変化の検知用・中身は外へ出さない）。
    pub fingerprint: String,
}

/// 解決の結果。
pub(crate) enum Resolved {
    Ok(Box<SubAuth>),
    /// テーブル viewer を失った。
    Revoked,
}

impl DataStore {
    /// 購読者の材料を `az` で解決する（購読の開始・TTL 切れ・無効化のたび）。
    pub(crate) async fn resolve_stream_auth(
        &self,
        ctx: &AuthContext,
        az: &AuthzMemo<'_>,
        table: &DataTable,
    ) -> Result<Resolved, DataError> {
        let viewer = az
            .check(
                ctx,
                Relation::Viewer,
                &ctx.ns().data_table(&table.id.to_string()),
            )
            .await?;
        if !viewer {
            return Ok(Resolved::Revoked);
        }
        let material = match &table.schema.row_policy {
            Some(p) => material::resolve(ctx, az, &[&p.read]).await?,
            None => Arc::new(PolicyMaterial::default()),
        };
        let masked = self.masked_fields(ctx, az, table).await?;
        // 個別共有集合は主体の全テーブル分なので、このテーブルの行だけに絞る（他テーブルの共有の
        // 変化でこの購読を落とさない・判定の集合も小さくなる）。
        let shared = self.shared_in_table(ctx, table, &material).await?;
        let fingerprint = fingerprint(&material, &shared, &masked);
        Ok(Resolved::Ok(Box::new(SubAuth {
            carry: None,
            resolved_at: Instant::now(),
            schema_version: table.schema_version,
            material,
            masked,
            shared,
            fingerprint,
        })))
    }
}

impl DataStore {
    /// 個別共有集合のうち、このテーブルに属する行の id。
    async fn shared_in_table(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        m: &PolicyMaterial,
    ) -> Result<HashSet<Uuid>, DataError> {
        if m.shared_record_ids.is_empty() || table.schema.row_policy.is_none() {
            return Ok(HashSet::new());
        }
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM data_record \
             WHERE tenant_int = $1 AND table_int = $2 AND tenant_id = $3 AND id = ANY($4)",
        )
        .bind(table.keys.tenant_int)
        .bind(table.keys.table_int)
        .bind(&ctx.tenant_id)
        .bind(&m.shared_record_ids)
        .fetch_all(self.pools.for_tenant(&ctx.tenant_id))
        .await
        .map_err(crate::map_db)?;
        Ok(ids.into_iter().collect())
    }
}

/// 材料の指紋（順序に依らない正規形の SHA-256）。
fn fingerprint(m: &PolicyMaterial, shared: &HashSet<Uuid>, masked: &HashSet<String>) -> String {
    let mut roles_direct: Vec<&String> = m.roles(false).iter().collect();
    roles_direct.sort();
    let mut roles_eff: Vec<&String> = m.roles(true).iter().collect();
    roles_eff.sort();
    let members: BTreeMap<&String, &bool> = m.role_members.iter().collect();
    let mut shared: Vec<&Uuid> = shared.iter().collect();
    shared.sort();
    let mut masked: Vec<&String> = masked.iter().collect();
    masked.sort();
    let canon = format!(
        "{roles_direct:?}|{roles_eff:?}|{members:?}|{shared:?}|{}|{masked:?}",
        m.shares_truncated
    );
    let d = Sha256::digest(canon.as_bytes());
    d.iter().fold(String::with_capacity(64), |mut out, b| {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn fingerprint_is_order_independent_and_sensitive() {
        let a = PolicyMaterial {
            roles_direct: Some(Arc::new(vec!["x".into(), "y".into()])),
            role_members: HashMap::from([("a".into(), true), ("b".into(), false)]),
            shared_record_ids: vec![Uuid::from_u128(2), Uuid::from_u128(1)],
            ..PolicyMaterial::default()
        };
        let b = PolicyMaterial {
            roles_direct: Some(Arc::new(vec!["y".into(), "x".into()])),
            role_members: HashMap::from([("b".into(), false), ("a".into(), true)]),
            shared_record_ids: vec![Uuid::from_u128(1), Uuid::from_u128(2)],
            ..PolicyMaterial::default()
        };
        let none = HashSet::new();
        let s12: HashSet<Uuid> = [Uuid::from_u128(1), Uuid::from_u128(2)].into();
        let s1: HashSet<Uuid> = [Uuid::from_u128(1)].into();
        assert_eq!(fingerprint(&a, &s12, &none), fingerprint(&b, &s12, &none));
        assert_ne!(
            fingerprint(&a, &s12, &none),
            fingerprint(&a, &s1, &none),
            "共有解除で変わる"
        );
        let masked: HashSet<String> = ["salary".to_string()].into();
        assert_ne!(
            fingerprint(&a, &s12, &none),
            fingerprint(&a, &s12, &masked),
            "マスクの変化で変わる"
        );
        let c = PolicyMaterial {
            roles_direct: Some(Arc::new(vec!["x".into()])),
            ..PolicyMaterial::default()
        };
        assert_ne!(
            fingerprint(&a, &s12, &none),
            fingerprint(&c, &s12, &none),
            "ロールで変わる"
        );
    }
}
