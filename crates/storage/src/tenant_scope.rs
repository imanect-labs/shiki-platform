//! テナント境界を持つテーブルの**単一定義**（#420）。
//!
//! テナント撤去（[`crate::StorageService::purge_tenant`]）とテナント移行（`shiki-admin retenant`）は
//! 「`tenant_id` を持つ全テーブル」を対象にする必要がある。両者はこれまで**テーブル名を手で列挙**して
//! いたため、新しいテーブルが増えるたびに漏れ、`tenant_id` を持つ 51 テーブルに対して撤去は 13・
//! 移行は 11 しか触っていなかった。結果として:
//!
//! - **撤去**: チャット履歴（`thread`/`message`/`generation_run`）・RAG の本文（`rag_chunk`）・
//!   構造化データ（`data_table`/`data_record`）・ワークフロー実行履歴・利用量が**削除されず残った**。
//!   「テナントを削除した」という約束（SAAS.2）を満たしていなかった。
//! - **移行**: 同じテーブル群が旧 `tenant_id` に取り残され、移行後のテナントからは消えたように見えた。
//!
//! そこで**一覧を持つのをやめ**、`information_schema` から実行時に導出する。持つのは
//! 「**対象外にするものとその理由**」だけ（[`PURGE_RETAINED`]）。新テーブルは自動的に対象に入る。
//! 導出結果が期待集合と一致することは結合テストが検査する（新テーブル追加時に CI が落ちる）。

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use sqlx::{PgPool, Postgres, Transaction};

use crate::error::StorageError;

/// テナント撤去で**削除しない**テーブルと、その理由。
///
/// ここに挙がっていない `tenant_id` 持ちテーブルはすべて削除対象になる（fail-closed 方向の既定）。
pub const PURGE_RETAINED: &[(&str, &str)] = &[
    (
        "audit_log",
        "削除の証跡として保持する（SAAS.2）。撤去そのものを記録する tenant.purge エントリもここに残る",
    ),
    (
        "tenant",
        "テナント台帳。行は tombstone（deleted）として残し、同名での再作成を検知する",
    ),
];

/// `tenant_id` 列を持つ実テーブルを、FK 依存の **子 → 親** 順で返す。
///
/// この順で `DELETE` すれば FK 違反を起こさない（子を先に消す）。`UPDATE`（移行）では
/// [`rename_tenant_rows`] が制約を commit 時検査へ遅延させるため順序は問わない。
pub async fn tenant_scoped_tables(pool: &PgPool) -> Result<Vec<String>, StorageError> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT c.table_name FROM information_schema.columns c \
         JOIN information_schema.tables t \
           ON t.table_schema = c.table_schema AND t.table_name = c.table_name \
         WHERE c.column_name = 'tenant_id' AND c.table_schema = 'public' \
           AND t.table_type = 'BASE TABLE' \
         ORDER BY c.table_name",
    )
    .fetch_all(pool)
    .await?;
    for t in &tables {
        // 導出したテーブル名は SQL へ直接埋め込むため、識別子として安全な形だけを通す
        // （information_schema 由来なので通常は安全だが、埋め込み側で担保しない設計にしない）。
        if !is_plain_ident(t) {
            return Err(StorageError::Integrity(format!(
                "テナント境界テーブル名が識別子として不正: {t}"
            )));
        }
    }
    let edges: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.conrelid::regclass::text, c.confrelid::regclass::text \
         FROM pg_constraint c \
         WHERE c.contype = 'f' AND c.conrelid <> c.confrelid \
           AND c.connamespace = 'public'::regnamespace",
    )
    .fetch_all(pool)
    .await?;
    topo_child_first(&tables, &edges)
}

/// テナント移行（rename）: `tenant_id` を持つ全テーブルの行を `from` → `to` へ付け替える。
///
/// **すべての遅延可能な制約を commit 時検査へ倒す**。参照列に `tenant_id` を含む FK は、親を先に
/// 更新しても子を先に更新しても途中状態で必ず違反するため（migration 0061 のコメント参照）、
/// 遅延が唯一の解。テーブル順は任意でよい。
///
/// 返すのは (テーブル名, 更新行数)。`ON UPDATE CASCADE` の子は親の更新で先に移るため、
/// 自身の `UPDATE` が 0 行になることがある（正常）。
pub async fn rename_tenant_rows(
    tx: &mut Transaction<'_, Postgres>,
    tables: &[String],
    from: &str,
    to: &str,
) -> Result<Vec<(String, u64)>, StorageError> {
    sqlx::query("SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut **tx)
        .await?;
    let mut moved = Vec::with_capacity(tables.len());
    for table in tables {
        let n = sqlx::query(&format!(
            "UPDATE {table} SET tenant_id = $2 WHERE tenant_id = $1"
        ))
        .bind(from)
        .bind(to)
        .execute(&mut **tx)
        .await?
        .rows_affected();
        moved.push((table.clone(), n));
    }
    Ok(moved)
}

/// 各テーブルの当該テナント行数を数える（dry-run レポート用）。0 件のテーブルも返す
/// ——「触れないテーブルがある」ことを運用者が確認できるようにするため。
pub async fn count_tenant_rows(
    pool: &PgPool,
    tables: &[String],
    tenant_id: &str,
) -> Result<Vec<(String, i64)>, StorageError> {
    let mut counts = Vec::with_capacity(tables.len());
    for table in tables {
        let n: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM {table} WHERE tenant_id = $1"
        ))
        .bind(tenant_id)
        .fetch_one(pool)
        .await?;
        counts.push((table.clone(), n));
    }
    Ok(counts)
}

/// 識別子として安全か（小文字英数と `_` のみ）。
fn is_plain_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// FK を辺（子 → 親）とみなし、子が親より先に来る順（＝削除順）へ並べる。
///
/// 対象集合の外を指す辺は無視する（例: `tenant_id` を持たないテーブルへの FK は、
/// テナント単位の削除では親が消えないので順序制約にならない）。
fn topo_child_first(
    tables: &[String],
    edges: &[(String, String)],
) -> Result<Vec<String>, StorageError> {
    let set: BTreeSet<&str> = tables.iter().map(String::as_str).collect();
    // parent → その親を消す前に消すべき子の数（indegree）。
    let mut indegree: BTreeMap<&str, usize> = tables.iter().map(|t| (t.as_str(), 0)).collect();
    let mut children_of: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (child, parent) in edges {
        let (c, p) = (child.as_str(), parent.as_str());
        if !set.contains(c) || !set.contains(p) {
            continue;
        }
        children_of.entry(c).or_default().push(p);
        *indegree.entry(p).or_insert(0) += 1;
    }
    // 誰からも参照されていないテーブル＝先に消してよい。BTreeMap 由来で順序は決定的。
    let mut queue: VecDeque<&str> = indegree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(t, _)| *t)
        .collect();
    let mut order: Vec<String> = Vec::with_capacity(tables.len());
    while let Some(t) = queue.pop_front() {
        order.push(t.to_string());
        for p in children_of.get(t).into_iter().flatten() {
            let d = indegree.entry(p).or_insert(0);
            *d -= 1;
            if *d == 0 {
                queue.push_back(p);
            }
        }
    }
    if order.len() != tables.len() {
        // FK の循環。テナント単位の一括削除では解けないので、黙って部分削除せず落とす。
        let stuck: Vec<&str> = tables
            .iter()
            .map(String::as_str)
            .filter(|t| !order.iter().any(|o| o == t))
            .collect();
        return Err(StorageError::Integrity(format!(
            "テナント境界テーブルの FK に循環があり削除順を決められません: {stuck:?}"
        )));
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }
    fn e(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| ((*a).to_string(), (*b).to_string()))
            .collect()
    }
    fn pos(order: &[String], name: &str) -> usize {
        order.iter().position(|t| t == name).expect("含まれる")
    }

    #[test]
    fn child_comes_before_parent() {
        let order = topo_child_first(
            &s(&["a", "b", "c"]),
            &e(&[("b", "a"), ("c", "b")]), // c → b → a
        )
        .expect("順序");
        assert!(pos(&order, "c") < pos(&order, "b"));
        assert!(pos(&order, "b") < pos(&order, "a"));
    }

    #[test]
    fn edges_outside_the_set_are_ignored() {
        // 親が対象外（tenant_id を持たない）なら順序制約にならず、全テーブルが並ぶ。
        let order = topo_child_first(&s(&["a"]), &e(&[("a", "outside")])).expect("順序");
        assert_eq!(order, s(&["a"]));
    }

    #[test]
    fn cycle_is_rejected_instead_of_partial_order() {
        // 循環を黙って無視して部分削除すると FK 違反か削除漏れになる。明示的に落とす。
        let err = topo_child_first(&s(&["a", "b"]), &e(&[("a", "b"), ("b", "a")]))
            .expect_err("循環は拒否される");
        assert!(matches!(err, StorageError::Integrity(_)));
    }

    #[test]
    fn rejects_unsafe_identifier() {
        assert!(is_plain_ident("node_share_link_grant"));
        assert!(!is_plain_ident("node; drop table x"));
        assert!(!is_plain_ident("Node"));
        assert!(!is_plain_ident(""));
    }
}
