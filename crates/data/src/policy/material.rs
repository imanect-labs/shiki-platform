//! 述語材料の解決（PIT-18・Task 9.3）。
//!
//! `$user.roles` や個別共有 ID 集合は OpenFGA 側にあり、行データは Postgres にある。
//! 1 クエリごとに全展開すると `IN(数千)` が肥大するため（PIT-1 と同型の
//! カーディナリティ問題）、次で抑える:
//!
//! - **上限**: ロール集合 [`MAX_ROLE_SET`]（超過は fail-closed エラー＝設定異常として拒否）、
//!   共有 ID [`MAX_SHARED_IDS`]（超過分は**落とす**＝可視が減る方向・`shares_truncated` で通知＋監査）。
//! - **キャッシュ**: TTL [`CACHE_TTL`] ＋ 世代カウンタ。共有/ロールのタプル書込チョーク
//!   ポイント（本 crate の共有 API）が世代を進めて即時失効させる（剥奪の反映遅延を
//!   TTL 上限でなくイベントで縛る）。
//! - SQL 側は `= ANY($n::text[])` の**配列バインド**で渡し、SQL テキストは肥大しない。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use authz::{AuthContext, AuthzClient, ObjectType, Relation};
use uuid::Uuid;

use crate::policy::ast::{PolicyExpr, PolicyOperand};
use crate::DataError;

/// ロール集合（直接/実効）の上限。超過は fail-closed（クエリ全体をエラー）。
pub(crate) const MAX_ROLE_SET: usize = 1_000;
/// 個別共有 ID 集合の上限。超過分は切り詰め（可視減方向）＋ `shares_truncated`。
pub(crate) const MAX_SHARED_IDS: usize = 10_000;
/// キャッシュ TTL（世代失効が主・TTL は上限の安全網）。
const CACHE_TTL: Duration = Duration::from_secs(30);

/// 解決済みの述語材料（1 リクエスト分・コンパイルは同期でこれを参照する）。
#[derive(Debug, Default)]
pub(crate) struct PolicyMaterial {
    /// `$user.roles`（直接タプルのみ・subtree=false）。
    pub roles_direct: Option<Arc<Vec<String>>>,
    /// `$user.roles`（継承展開済みの実効集合・subtree=true）。
    pub roles_effective: Option<Arc<Vec<String>>>,
    /// 個別共有された data_record の local id 集合（viewer/editor の直接タプル）。
    pub shared_record_ids: Vec<Uuid>,
    /// 共有集合が上限で切り詰められたか（応答へ伝搬し監査に残す）。
    pub shares_truncated: bool,
}

impl PolicyMaterial {
    /// HasRole / UserRoles の判定用ロール集合。
    pub fn roles(&self, subtree: bool) -> &[String] {
        let set = if subtree {
            self.roles_effective.as_ref()
        } else {
            self.roles_direct.as_ref()
        };
        set.map_or(&[][..], |v| v.as_slice())
    }
}

/// AST が必要とする材料の解析結果。
#[derive(Debug, Default, Clone, Copy)]
struct Needs {
    roles_direct: bool,
    roles_effective: bool,
}

fn analyze(expr: &PolicyExpr, needs: &mut Needs) {
    match expr {
        PolicyExpr::Any(children) | PolicyExpr::All(children) => {
            for c in children {
                analyze(c, needs);
            }
        }
        PolicyExpr::HasRole { subtree, .. } => {
            if *subtree {
                needs.roles_effective = true;
            } else {
                needs.roles_direct = true;
            }
        }
        PolicyExpr::FieldCmp {
            value: PolicyOperand::UserRoles { subtree },
            ..
        } => {
            if *subtree {
                needs.roles_effective = true;
            } else {
                needs.roles_direct = true;
            }
        }
        _ => {}
    }
}

/// キャッシュキー（tenant × user × 種別）。
type Key = (String, String, u8);
const KIND_ROLES_DIRECT: u8 = 0;
const KIND_ROLES_EFFECTIVE: u8 = 1;
const KIND_SHARED_RECORDS: u8 = 2;

struct Entry {
    at: Instant,
    generation: u64,
    values: Arc<Vec<String>>,
}

/// TTL＋世代付きの材料キャッシュ（外部依存を増やさない自前実装・プロセス内）。
pub(crate) struct MaterialCache {
    map: RwLock<HashMap<Key, Entry>>,
    generation: AtomicU64,
}

impl Default for MaterialCache {
    fn default() -> Self {
        MaterialCache {
            map: RwLock::new(HashMap::new()),
            generation: AtomicU64::new(0),
        }
    }
}

impl MaterialCache {
    /// 共有/ロールのタプルが変わったら呼ぶ（全キャッシュを世代で即時失効）。
    pub fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        // エントリ自体は get 時の世代比較で弾かれる（遅延掃除）。肥大防止に都度クリア。
        if let Ok(mut map) = self.map.write() {
            map.clear();
        }
    }

    fn get(&self, key: &Key) -> Option<Arc<Vec<String>>> {
        let generation = self.generation.load(Ordering::SeqCst);
        let map = self.map.read().ok()?;
        let e = map.get(key)?;
        if e.generation == generation && e.at.elapsed() < CACHE_TTL {
            Some(Arc::clone(&e.values))
        } else {
            None
        }
    }

    fn put(&self, key: Key, values: Arc<Vec<String>>) {
        let generation = self.generation.load(Ordering::SeqCst);
        if let Ok(mut map) = self.map.write() {
            map.insert(
                key,
                Entry {
                    at: Instant::now(),
                    generation,
                    values,
                },
            );
        }
    }
}

/// 述語が要求する材料を（キャッシュ経由で）解決する。
///
/// `exprs` はこのクエリで合成される全式（read または write）。共有 ID 集合は
/// 式に依らず常に解決する（`OR id = ANY(共有)` の枝が必ず付くため）。
pub(crate) async fn resolve(
    ctx: &AuthContext,
    authz: &dyn AuthzClient,
    cache: &MaterialCache,
    exprs: &[&PolicyExpr],
) -> Result<PolicyMaterial, DataError> {
    let mut needs = Needs::default();
    for e in exprs {
        analyze(e, &mut needs);
    }

    let mut material = PolicyMaterial::default();

    if needs.roles_direct {
        material.roles_direct = Some(
            cached_role_set(ctx, authz, cache, KIND_ROLES_DIRECT, false).await?,
        );
    }
    if needs.roles_effective {
        material.roles_effective = Some(
            cached_role_set(ctx, authz, cache, KIND_ROLES_EFFECTIVE, true).await?,
        );
    }

    // 個別共有集合（viewer/editor の直接タプル。読取はどちらでも可視になる）。
    let shared = cached_shared_records(ctx, authz, cache).await?;
    let mut ids: Vec<Uuid> = Vec::with_capacity(shared.len().min(MAX_SHARED_IDS));
    for raw in shared.iter() {
        if let Ok(id) = Uuid::parse_str(raw) {
            if ids.len() >= MAX_SHARED_IDS {
                material.shares_truncated = true;
                tracing::warn!(
                    user = %ctx.principal.id,
                    limit = MAX_SHARED_IDS,
                    "個別共有集合が上限を超過（超過分は不可視・PIT-18 fail-closed）"
                );
                break;
            }
            ids.push(id);
        }
    }
    material.shared_record_ids = ids;
    Ok(material)
}

/// ロール集合（direct=read_subject_objects / effective=list_objects）を上限付きで解決する。
async fn cached_role_set(
    ctx: &AuthContext,
    authz: &dyn AuthzClient,
    cache: &MaterialCache,
    kind: u8,
    effective: bool,
) -> Result<Arc<Vec<String>>, DataError> {
    let key: Key = (ctx.tenant_id.clone(), ctx.principal.id.clone(), kind);
    if let Some(hit) = cache.get(&key) {
        return Ok(hit);
    }
    let raw = if effective {
        authz
            .list_objects(&ctx.subject(), Relation::Member, ObjectType::Role)
            .await
    } else {
        authz
            .read_subject_objects(&ctx.subject(), ObjectType::Role)
            .await
    }
    .map_err(|e| DataError::Internal(format!("authz: {e}")))?;
    if raw.len() > MAX_ROLE_SET {
        // ロール爆発は述語の部分適用（見えすぎ/見えなさすぎの不定）を招くため
        // fail-closed でクエリごと拒否する（PIT-18: 閾値超のフォールバックを仕様化）。
        return Err(DataError::Invalid(format!(
            "実行主体のロール集合が上限（{MAX_ROLE_SET}）を超えています。管理者にロール構成の見直しを依頼してください"
        )));
    }
    let ns = ctx.ns();
    let mut roles = Vec::with_capacity(raw.len());
    for o in raw {
        let Some((_, id_part)) = o.split_once(':') else {
            continue;
        };
        if let Some(local) = ns.strip_object_id(id_part) {
            roles.push(local.to_string());
        }
    }
    let arc = Arc::new(roles);
    cache.put(key, Arc::clone(&arc));
    Ok(arc)
}

/// 個別共有された data_record の直接タプル集合（local id 文字列）。
async fn cached_shared_records(
    ctx: &AuthContext,
    authz: &dyn AuthzClient,
    cache: &MaterialCache,
) -> Result<Arc<Vec<String>>, DataError> {
    let key: Key = (
        ctx.tenant_id.clone(),
        ctx.principal.id.clone(),
        KIND_SHARED_RECORDS,
    );
    if let Some(hit) = cache.get(&key) {
        return Ok(hit);
    }
    let raw = authz
        .read_subject_objects(&ctx.subject(), ObjectType::DataRecord)
        .await
        .map_err(|e| DataError::Internal(format!("authz: {e}")))?;
    let ns = ctx.ns();
    let mut ids = Vec::with_capacity(raw.len());
    for o in raw {
        let Some((_, id_part)) = o.split_once(':') else {
            continue;
        };
        if let Some(local) = ns.strip_object_id(id_part) {
            ids.push(local.to_string());
        }
    }
    let arc = Arc::new(ids);
    cache.put(key, Arc::clone(&arc));
    Ok(arc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyze_detects_role_needs() {
        let mut needs = Needs::default();
        analyze(
            &PolicyExpr::Any(vec![
                PolicyExpr::HasRole {
                    role: "a".into(),
                    subtree: true,
                },
                PolicyExpr::FieldCmp {
                    field: "dept".into(),
                    op: crate::policy::ast::CmpOp::In,
                    value: PolicyOperand::UserRoles { subtree: false },
                },
            ]),
            &mut needs,
        );
        assert!(needs.roles_effective);
        assert!(needs.roles_direct);
    }

    #[test]
    fn cache_ttl_and_generation() {
        let cache = MaterialCache::default();
        let key: Key = ("t".into(), "u".into(), 0);
        cache.put(key.clone(), Arc::new(vec!["r1".into()]));
        assert!(cache.get(&key).is_some());
        // 世代を進めると即時失効する（共有/剥奪の即時反映）。
        cache.invalidate();
        assert!(cache.get(&key).is_none());
    }
}
