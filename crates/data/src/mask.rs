//! フィールドマスク（Task 9.4・PIT-19）。
//!
//! 「表示を隠す」と「検索に使わせない」を**同一の判定**から強制する:
//! - 投影: 全読取応答（get/list/query/リビジョン差分/lookup 射影）から対象フィールドを除去
//! - 問い合わせ: filter/sort/group_by/metrics が対象フィールドを参照したら**実行前に 403**
//! - 書込: 読めないフィールドへの書込も拒否（盲目上書きの防止）
//! - 派生: 読めない列に依存する派生列（数式・lookup・rollup）も読めない列として扱う（空の列を見せない）
//!
//! `readable_by` はロール/ユーザーレベルの式（has_role/public とその any/all 合成）のみで、
//! リクエスト単位に一度だけホスト側で評価する（行の値に依存しない＝保存ビューでも
//! 実行時の閲覧者基準で再評価される）。

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;

use authz::AuthContext;
use serde_json::Value;
use uuid::Uuid;

use crate::derived::MAX_RESOLVE_DEPTH;
use crate::formula::FormulaExpr;
use crate::model::{DataRecord, DataTable, FieldDef, FieldPatch, TableSchema};
use crate::policy::ast::PolicyExpr;
use crate::policy::material::{self, PolicyMaterial};
use crate::policy::memo::AuthzMemo;
use crate::store::DataStore;
use crate::DataError;

/// ロール/ユーザーレベル式の評価（マスク判定・リクエストで一度だけ）。
///
/// 行依存の式（field_cmp / is_owner）はスキーマ検証で拒否済み。防御的に**マスク側に倒す**
/// （false = 読めない）。
fn eval_role_level(expr: &PolicyExpr, material: &PolicyMaterial) -> bool {
    match expr {
        PolicyExpr::Public => true,
        PolicyExpr::HasRole { role, subtree } => material.has_role(role, *subtree),
        PolicyExpr::Any(children) => children.iter().any(|c| eval_role_level(c, material)),
        PolicyExpr::All(children) => children.iter().all(|c| eval_role_level(c, material)),
        PolicyExpr::FieldCmp { .. } | PolicyExpr::IsOwner => false,
    }
}

type BoxFut<'a, T> = Pin<Box<dyn Future<Output = Result<T, DataError>> + Send + 'a>>;

/// 派生列が依存する列（自テーブルの参照名・参照先テーブルの参照名）。
#[derive(Debug, Default)]
struct DerivedDeps<'a> {
    local: Vec<&'a str>,
    remote: Vec<(Uuid, &'a str)>,
}

/// 派生列の依存（数式の列・式内ロールアップ・計算の列・lookup / rollup の経由リンクと参照先の列）。
fn derived_deps<'a>(schema: &'a TableSchema, f: &'a FieldDef) -> DerivedDeps<'a> {
    let mut d = DerivedDeps::default();
    let via = |d: &mut DerivedDeps<'a>, link: &'a str, target: Option<&'a str>| {
        d.local.push(link);
        if let (Some(rt), Some(t)) = (schema.field(link).and_then(|l| l.ref_table), target) {
            d.remote.push((rt, t));
        }
    };
    if let Some(l) = &f.lookup {
        via(&mut d, &l.via_field, Some(&l.target_field));
    }
    if let Some(r) = &f.rollup {
        via(&mut d, &r.link, r.target.as_deref());
    }
    if let Some(c) = &f.computed {
        d.local.extend(c.fields.iter().map(String::as_str));
    }
    if let Some(e) = &f.formula {
        d.local.extend(e.local_refs());
        for n in e.rollups() {
            if let FormulaExpr::Rollup { link, target, .. } = n {
                via(&mut d, link, target.as_deref());
            }
        }
    }
    d
}

impl DataStore {
    /// 実行主体に対して**読めない**フィールドの参照名集合（グラウンディング用・Task 15.7）。
    ///
    /// チャットの `data_schema` のように「閲覧者に見える列だけを示す」面が使う。判定はクエリの
    /// 投影・検索拒否と同じ（[`Self::masked_fields`]）で、認可材料はこの呼び出しの中だけで使い捨てる
    /// （リクエストを跨いで保持しない・PIT-18）。
    pub async fn masked_field_keys(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
    ) -> Result<HashSet<String>, DataError> {
        self.masked_fields(ctx, &self.authz_memo(), table).await
    }

    /// 実行主体に対して**読めない**フィールド名集合を返す（field_policy 未定義なら空）。
    ///
    /// `field_policy` で直接隠れる列に加え、**読めない列に依存する派生列**（数式・計算・ルックアップ・
    /// ロールアップ。経由するリンク列が読めない・参照先で辿る列が閲覧者に読めない）も同じ集合に入れる。
    /// 「値なし」で計算した空の列を見せず、投影・検索拒否・書込拒否・SSE・ビューの全面で同じ扱いになる。
    pub(crate) async fn masked_fields(
        &self,
        ctx: &AuthContext,
        az: &AuthzMemo<'_>,
        table: &DataTable,
    ) -> Result<HashSet<String>, DataError> {
        self.masked_fields_at(ctx, az, table, 0).await
    }

    fn masked_fields_at<'a>(
        &'a self,
        ctx: &'a AuthContext,
        az: &'a AuthzMemo<'_>,
        table: &'a DataTable,
        depth: usize,
    ) -> BoxFut<'a, HashSet<String>> {
        Box::pin(async move {
            let mut masked: HashSet<String> = if table.schema.field_policy.is_empty() {
                HashSet::new()
            } else {
                let exprs: Vec<&PolicyExpr> = table
                    .schema
                    .field_policy
                    .iter()
                    .map(|p| &p.readable_by)
                    .collect();
                let m = material::resolve_roles(ctx, az, &exprs).await?;
                table
                    .schema
                    .field_policy
                    .iter()
                    .filter(|p| !eval_role_level(&p.readable_by, &m))
                    .map(|p| p.field.clone())
                    .collect()
            };
            let deps: Vec<(&FieldDef, DerivedDeps<'_>)> = table
                .schema
                .fields
                .iter()
                .filter(|f| f.field_type.is_derived())
                .map(|f| (f, derived_deps(&table.schema, f)))
                .collect();
            if deps.is_empty() {
                return Ok(masked);
            }
            // 参照先で辿る列が閲覧者に読めないか（参照先ごとに 1 回・段数の上限を超えた先は見ない）。
            let mut remote: HashMap<Uuid, HashSet<String>> = HashMap::new();
            if depth < MAX_RESOLVE_DEPTH {
                let tables: HashSet<Uuid> = deps
                    .iter()
                    .flat_map(|(_, d)| d.remote.iter().map(|(t, _)| *t))
                    .filter(|t| *t != table.id)
                    .collect();
                for t in tables {
                    let rt = match self.fetch_live(ctx, t).await {
                        Ok(rt) => rt,
                        Err(DataError::NotFound) => continue,
                        Err(e) => return Err(e),
                    };
                    let m = self.masked_fields_at(ctx, az, &rt, depth + 1).await?;
                    remote.insert(t, m);
                }
            }
            // 自テーブル内の派生列どうしの依存は不動点まで回す（循環は保存時に拒否済み・有限回で止まる）。
            loop {
                let mut grew = false;
                for (f, d) in &deps {
                    if masked.contains(&f.key) {
                        continue;
                    }
                    let hit = d.local.iter().any(|k| masked.contains(*k))
                        || d.remote.iter().any(|(t, k)| {
                            let own = (*t == table.id).then_some(&masked);
                            own.or_else(|| remote.get(t))
                                .is_some_and(|m| m.contains(*k))
                        });
                    if hit {
                        masked.insert(f.key.clone());
                        grew = true;
                    }
                }
                if !grew {
                    break;
                }
            }
            Ok(masked)
        })
    }

    /// 読取応答の出口（閲覧者本人の権限での派生値・リンク値の解決 → マスク列の除去）。
    ///
    /// レコードを外へ返す面（get・query・SQL・SSE・遷移や更新の応答）はこの順で掛ける。リンク列の
    /// 値もここで閲覧者に見える参照先だけに絞られる（[`DataStore::resolve_derived_fields`]・§7.2）。
    pub(crate) async fn project_read(
        &self,
        ctx: &AuthContext,
        az: &AuthzMemo<'_>,
        table: &DataTable,
        masked: &HashSet<String>,
        records: &mut [DataRecord],
    ) -> Result<(), DataError> {
        self.resolve_derived_fields(ctx, az, table, masked, records)
            .await?;
        Self::apply_mask_records(masked, records);
        Ok(())
    }

    /// 読取応答の投影段（レコード群からマスク対象フィールドを除去する単一点）。
    pub(crate) fn apply_mask_records(masked: &HashSet<String>, records: &mut [DataRecord]) {
        if masked.is_empty() {
            return;
        }
        for r in records {
            if let Some(obj) = r.data.as_object_mut() {
                obj.retain(|k, _| !masked.contains(k));
            }
            r.suppressed_fields.retain(|k| !masked.contains(k));
        }
    }

    /// リビジョン差分の投影段（マスク対象フィールドの差分を落とす）。
    ///
    /// 値を伏せて差分行を残すと「読めない列がいつ変わったか」が閲覧者に分かる（B-P3-1）。読めない列の
    /// 変更記録は閲覧者に返さない（監査ログには残る）。読めない列だけが変わった版は呼び出し側が落とす。
    pub(crate) fn apply_mask_patches(masked: &HashSet<String>, patches: &mut Vec<FieldPatch>) {
        if masked.is_empty() {
            return;
        }
        patches.retain(|p| !masked.contains(&p.field));
    }

    /// 書込面の単一点: 書き手が読めない（マスクされた）列への書込を 403 にする。
    ///
    /// 作成（1 件・一括取込の開始と検証・CSV・ワークフロー・チャット・スクリプト・ゲートウェイ）と
    /// 更新（1 件・一括 upsert）の**全経路がこの関数を通る**。作成を素通しにすると、読めない列の
    /// unique 違反（409）から既存行の値を当てられる（存在オラクル・PIT-19）。返り値は判定に使った
    /// マスク集合（応答の投影に使い回す）。
    pub(crate) async fn ensure_fields_writable(
        &self,
        ctx: &AuthContext,
        az: &AuthzMemo<'_>,
        table: &DataTable,
        payload: Option<&serde_json::Map<String, Value>>,
    ) -> Result<HashSet<String>, DataError> {
        let masked = self.masked_fields(ctx, az, table).await?;
        if let Some(p) = payload {
            Self::ensure_writable_fields(&masked, p)?;
        }
        Ok(masked)
    }

    /// 書込面の検査: 読めないフィールドへの書込を拒否（盲目上書き防止）。
    pub(crate) fn ensure_writable_fields(
        masked: &HashSet<String>,
        payload: &serde_json::Map<String, Value>,
    ) -> Result<(), DataError> {
        for key in payload.keys() {
            if masked.contains(key) {
                return Err(DataError::Forbidden);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn material(roles: &[&str]) -> PolicyMaterial {
        PolicyMaterial {
            roles_direct: Some(Arc::new(roles.iter().map(|s| (*s).into()).collect())),
            roles_effective: Some(Arc::new(roles.iter().map(|s| (*s).into()).collect())),
            shared_record_ids: vec![],
            shares_truncated: false,
            role_members: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn role_level_eval() {
        let keiri = PolicyExpr::HasRole {
            role: "keiri".into(),
            subtree: true,
        };
        assert!(eval_role_level(&keiri, &material(&["keiri"])));
        assert!(!eval_role_level(&keiri, &material(&[])));
        assert!(eval_role_level(&PolicyExpr::Public, &material(&[])));
        // 行依存式は防御的に「読めない」へ倒す。
        assert!(!eval_role_level(
            &PolicyExpr::IsOwner,
            &material(&["keiri"])
        ));
        let any = PolicyExpr::Any(vec![keiri, PolicyExpr::Public]);
        assert!(eval_role_level(&any, &material(&[])));
    }

    #[test]
    fn patches_drop_masked_fields_entirely() {
        let mut patches = vec![
            FieldPatch {
                field: "salary".into(),
                old: serde_json::json!(100),
                new: serde_json::json!(200),
            },
            FieldPatch {
                field: "name".into(),
                old: serde_json::json!("a"),
                new: serde_json::json!("b"),
            },
        ];
        let masked: HashSet<String> = ["salary".to_string()].into();
        DataStore::apply_mask_patches(&masked, &mut patches);
        // 値を伏せるだけでなく、変更の記録（列名）ごと返さない。
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].field, "name");
    }
}
