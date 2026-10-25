//! RAG の post-filter: 候補 `record_id` を閲覧者の可視集合へ絞る（Task 15.8・data-platform §11.1）。
//!
//! 1 テーブルにつき**クエリ 1 本**。テーブル viewer（第 1 層・OpenFGA の既存タプル）を確かめたうえで、
//! 行述語（第 2 層・v1 の意味論）と個別共有（第 4 層・既存の `data_record` タプル）を REST の読取と
//! 同じ合成（[`crate::DataStore::select_visible_projection`]）で効かせる。**OpenFGA のタプルは増やさない**。
//! 第 3 層（フィールドマスク）は投影に含められない（スキーマ検証）ので、ここでは再確認だけする。

use std::collections::HashSet;
use storage::audit::AuditAction;

use authz::AuthContext;
use uuid::Uuid;

use super::projection::{generation, projected, projected_field_keys, render};
use crate::store::DataStore;
use crate::DataError;

/// 1 回の post-filter に渡せる候補数の上限（RAG の `MAX_FETCH_K` と同じ桁）。
pub const MAX_RAG_POST_FILTER_IDS: usize = 256;

/// 可視と判定されたレコード 1 件（**現在の値から作り直した**投影）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RagRecordHit {
    pub record_id: Uuid,
    pub rev: i64,
    pub title: String,
    pub body: String,
}

/// post-filter の結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RagVisibleRecords {
    /// テーブルの現在の索引世代（オプトインしていない・見えない・存在しないテーブルは `None`）。
    /// 呼び出し側は**チャンクの世代と一致しない候補を落とす**（field_policy 後付け直後の古いチャンク）。
    pub generation: Option<String>,
    /// 可視な候補（入力の順序は保たない）。不可視と不存在は区別しない。
    pub records: Vec<RagRecordHit>,
}

impl DataStore {
    /// 検索候補のレコード id を閲覧者の可視集合に絞る（RAG の post-filter・Task 15.8）。
    ///
    /// - テーブル viewer を持たない・テーブルが無い・オプトインしていない → **全件を落とす**（エラーに
    ///   しない。複数テーブルを跨ぐ検索で 1 テーブルの剥奪が検索全体を失敗させないため）
    /// - 行述語・個別共有は REST の読取と同じ合成で 1 本のクエリに載せる（`MAX_ROLE_SET` 超過などの
    ///   材料解決の失敗は REST と同じくエラーで返す＝fail-closed）
    /// - 投影列が閲覧者のマスク列と交差したら（スキーマ検証が壊れていたら）全件を落とす
    ///
    /// 認可材料はこの呼び出しの中だけで使い捨てる（PIT-18）。
    pub async fn rag_visible_records(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        candidates: &[Uuid],
        trace_id: Option<&str>,
    ) -> Result<RagVisibleRecords, DataError> {
        let mut seen = HashSet::new();
        let ids: Vec<Uuid> = candidates
            .iter()
            .copied()
            .filter(|id| seen.insert(*id))
            .collect();
        if ids.len() > MAX_RAG_POST_FILTER_IDS {
            return Err(DataError::Invalid(format!(
                "post-filter の候補は {MAX_RAG_POST_FILTER_IDS} 件までです"
            )));
        }
        let az = self.authz_memo();
        let table = match self
            .open_for_read(ctx, &az, table_id, AuditAction::DataRagPostFilter, trace_id)
            .await
        {
            Ok(t) => t,
            Err(DataError::Forbidden | DataError::NotFound) => {
                return Ok(RagVisibleRecords::default())
            }
            Err(e) => return Err(e),
        };
        let (Some(proj), Some(cols), Some(generation)) = (
            table.schema.rag.as_ref(),
            projected(&table.schema),
            generation(&table.schema),
        ) else {
            return Ok(RagVisibleRecords::default());
        };
        // 第 3 層の再確認（スキーマ検証と二重）。投影列がこの閲覧者に対してマスクされていたら値を出さない。
        let masked = self.masked_fields(ctx, &az, &table).await?;
        if projected_field_keys(proj).any(|k| masked.contains(k)) {
            tracing::error!(
                %table_id,
                "RAG の投影列がフィールドマスクと交差しています（スキーマ検証の不整合）。全件を落とします"
            );
            return Ok(RagVisibleRecords {
                generation: Some(generation),
                records: Vec::new(),
            });
        }
        let keys: Vec<String> = cols.iter().map(|(k, _)| k.clone()).collect();
        let rows = self
            .select_visible_projection(ctx, &az, &table, &ids, &keys)
            .await?;
        let records = rows
            .into_iter()
            .map(|(record_id, rev, values)| {
                let r = render(&cols, &values.0);
                RagRecordHit {
                    record_id,
                    rev,
                    title: r.title,
                    body: r.body,
                }
            })
            .collect();
        Ok(RagVisibleRecords {
            generation: Some(generation),
            records,
        })
    }
}
