//! ワークフロー一覧の要約（Task 10.14・一覧 API の単一クエリ射影）。
//!
//! 認可済み id 集合（artifact の所有∪共有・API 層が解決）を受け取り、**単一 SQL** で
//! 最新版 body の表示情報（display_name/description/トリガ種）と registration 状態を射影する。
//! body 全体・IR 本文は運ばない（必要フィールドのみ・パフォーマンス規約）。

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// 一覧 1 行の要約。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WorkflowSummary {
    pub id: Uuid,
    pub name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub current_version: i64,
    /// IR triggers[] の kind 列（schedule/event/interactive・重複除去なし）。
    pub trigger_kinds: Vec<String>,
    /// enabled / disabled / suspended_reconsent / none（未登録）。
    pub enabled_status: String,
    pub enabled_version: Option<i64>,
    /// 自動実行の起動を見送り続けているか（登録の API の `launch_skip` と同じ条件・GAP-67）。
    pub launch_skipped: bool,
    pub updated_at: DateTime<Utc>,
}

/// 一覧要約の読み取り（読み取り専用・認可は呼び出し側で解決済みの id 集合が前提）。
#[derive(Clone)]
pub struct WorkflowSummaryStore {
    db: PgPool,
}

impl WorkflowSummaryStore {
    pub fn new(db: PgPool) -> Self {
        WorkflowSummaryStore { db }
    }

    /// id 集合の要約を更新日降順で返す（id は認可済み前提・tenant 二重フィルタ）。
    pub async fn list(
        &self,
        tenant_id: &str,
        ids: &[Uuid],
    ) -> Result<Vec<WorkflowSummary>, sqlx::Error> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as(&format!(
            "SELECT a.id, a.name, \
                    v.body->>'display_name' AS display_name, \
                    v.body->>'description' AS description, \
                    a.current_version, \
                    COALESCE( \
                      (SELECT array_agg(t->>'kind') \
                       FROM jsonb_array_elements(v.body->'triggers') AS t), \
                      '{{}}') AS trigger_kinds, \
                    COALESCE(r.status, 'none') AS enabled_status, \
                    r.enabled_version, \
                    EXISTS (SELECT 1 FROM workflow_launch_skip s \
                            WHERE s.tenant_id = a.tenant_id AND s.workflow_id = a.id \
                              AND {current_skip}) AS launch_skipped, \
                    a.updated_at \
             FROM artifact a \
             JOIN artifact_version v \
               ON v.tenant_id = a.tenant_id AND v.artifact_id = a.id \
              AND v.version = a.current_version \
             LEFT JOIN workflow_registration r \
               ON r.tenant_id = a.tenant_id AND r.workflow_id = a.id \
             WHERE a.tenant_id = $1 AND a.id = ANY($2) AND a.deleted_at IS NULL \
             ORDER BY a.updated_at DESC, a.id DESC",
            current_skip = crate::run::launch_skip::CURRENT_SKIP,
        ))
        .bind(tenant_id)
        .bind(ids)
        .fetch_all(&self.db)
        .await
    }

    /// id 集合の実行の入力の項目（現行版の `input_schema` の `properties` のキーと `required`・id は認可済み前提）。
    ///
    /// 本文全体は運ばない（キーの配列だけを SQL で取り出す）。キーの並びは `x-order`（画面に並べる順）にあるものを
    /// その順に先に、残りを名前の順に後ろに置く。
    pub async fn inputs(
        &self,
        tenant_id: &str,
        ids: &[Uuid],
    ) -> Result<Vec<WorkflowInputs>, sqlx::Error> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<InputsRow> = sqlx::query_as(
            "SELECT a.id, \
                    COALESCE((SELECT array_agg(k ORDER BY k) FROM jsonb_object_keys( \
                        CASE WHEN jsonb_typeof(v.body->'input_schema'->'properties') = 'object' \
                             THEN v.body->'input_schema'->'properties' ELSE '{}'::jsonb END) AS k), \
                      '{}') AS keys, \
                    COALESCE((SELECT array_agg(o) FROM jsonb_array_elements_text( \
                        CASE WHEN jsonb_typeof(v.body->'input_schema'->'x-order') = 'array' \
                             THEN v.body->'input_schema'->'x-order' ELSE '[]'::jsonb END) AS o), \
                      '{}') AS ordered, \
                    COALESCE((SELECT array_agg(r) FROM jsonb_array_elements_text( \
                        CASE WHEN jsonb_typeof(v.body->'input_schema'->'required') = 'array' \
                             THEN v.body->'input_schema'->'required' ELSE '[]'::jsonb END) AS r), \
                      '{}') AS required \
             FROM artifact a \
             JOIN artifact_version v \
               ON v.tenant_id = a.tenant_id AND v.artifact_id = a.id \
              AND v.version = a.current_version \
             WHERE a.tenant_id = $1 AND a.id = ANY($2) AND a.deleted_at IS NULL",
        )
        .bind(tenant_id)
        .bind(ids)
        .fetch_all(&self.db)
        .await?;
        Ok(rows.into_iter().map(InputsRow::into_inputs).collect())
    }
}

/// 実行の入力の項目（[`WorkflowSummaryStore::inputs`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowInputs {
    pub id: Uuid,
    /// `properties` のキー（`x-order` の順に先に、残りを名前の順に）。
    pub keys: Vec<String>,
    /// `required` のキー（`properties` にあるものだけ）。
    pub required: Vec<String>,
}

#[derive(sqlx::FromRow)]
struct InputsRow {
    id: Uuid,
    keys: Vec<String>,
    ordered: Vec<String>,
    required: Vec<String>,
}

impl InputsRow {
    fn into_inputs(self) -> WorkflowInputs {
        let mut keys: Vec<String> =
            self.ordered
                .iter()
                .filter(|k| self.keys.contains(k))
                .fold(Vec::new(), |mut acc, k| {
                    if !acc.contains(k) {
                        acc.push(k.clone());
                    }
                    acc
                });
        keys.extend(
            self.keys
                .iter()
                .filter(|k| !self.ordered.contains(k))
                .cloned(),
        );
        let required = self
            .required
            .into_iter()
            .filter(|r| keys.contains(r))
            .collect();
        WorkflowInputs {
            id: self.id,
            keys,
            required,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 入力の項目は x-order の順に先に、残りを名前の順に。x-order の重複と properties に無いキーは落とす。
    #[test]
    fn input_keys_follow_x_order_then_name() {
        let got = InputsRow {
            id: Uuid::nil(),
            keys: vec!["amount".into(), "memo".into(), "title".into()],
            ordered: vec![
                "title".into(),
                "ghost".into(),
                "title".into(),
                "amount".into(),
            ],
            required: vec!["amount".into(), "ghost".into()],
        }
        .into_inputs();
        assert_eq!(got.keys, ["title", "amount", "memo"]);
        assert_eq!(got.required, ["amount"]);
    }
}
