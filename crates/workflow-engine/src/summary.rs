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
    pub updated_at: DateTime<Utc>,
}

/// 名前検索の 1 件（AI の `search_workflows`・図のボタンに書く名前を引くため）。
///
/// 一覧要約に、ボタンのフォームを組むための `input_schema` だけを足した射影（IR 本文は運ばない）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WorkflowSearchHit {
    pub id: Uuid,
    /// artifact の名前（＝図のボタン `-> workflow 名前` が解決する文字列）。
    pub name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    /// IR triggers[] の kind 列（schedule/event/interactive）。
    pub trigger_kinds: Vec<String>,
    /// 対話トリガの入力スキーマ（JSON Schema サブセット・未定義は `None`）。
    pub input_schema: Option<serde_json::Value>,
    /// enabled / disabled / suspended_reconsent / none（未登録）。
    pub enabled_status: String,
    /// 検索した本人の所有か（false は共有されただけ＝他人が作ったもの）。
    pub owned: bool,
    pub updated_at: DateTime<Utc>,
}

/// ワークフロー 1 つの、直近に終わった実行の件数（ホームの「今日のこと」・#965）。
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct WorkflowRunCount {
    pub workflow_id: Uuid,
    /// 期間内に成功で終わった実行の数。
    pub succeeded: i64,
    /// 期間内に失敗で終わった実行の数。
    pub failed: i64,
}

/// 失敗した実行 1 件（どのワークフローの・いつの・なぜ）。
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct FailedRun {
    pub run_id: Uuid,
    pub workflow_id: Uuid,
    pub workflow_name: String,
    pub display_name: Option<String>,
    /// 失敗の理由（先頭 200 文字まで・一覧に添えるだけなので全文は運ばない）。
    pub fail_reason: Option<String>,
    pub finished_at: DateTime<Utc>,
}

/// 検索語を ILIKE の部分一致パターンへ（空白区切りの語ごと・ワイルドカードは文字として扱う）。
///
/// 利用者・モデルが書いた `%`/`_` がワイルドカードとして効くと「全件一致」になるため、
/// `\` でエスケープする（SQL 側は `ESCAPE '\'`）。空の語は捨てる。
#[must_use]
pub fn like_patterns(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|term| {
            let mut p = String::with_capacity(term.len() + 2);
            p.push('%');
            for c in term.chars() {
                if matches!(c, '%' | '_' | '\\') {
                    p.push('\\');
                }
                p.push(c);
            }
            p.push('%');
            p
        })
        .collect()
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
        sqlx::query_as(
            "SELECT a.id, a.name, \
                    v.body->>'display_name' AS display_name, \
                    v.body->>'description' AS description, \
                    a.current_version, \
                    COALESCE( \
                      (SELECT array_agg(t->>'kind') \
                       FROM jsonb_array_elements(v.body->'triggers') AS t), \
                      '{}') AS trigger_kinds, \
                    COALESCE(r.status, 'none') AS enabled_status, \
                    r.enabled_version, \
                    a.updated_at \
             FROM artifact a \
             JOIN artifact_version v \
               ON v.tenant_id = a.tenant_id AND v.artifact_id = a.id \
              AND v.version = a.current_version \
             LEFT JOIN workflow_registration r \
               ON r.tenant_id = a.tenant_id AND r.workflow_id = a.id \
             WHERE a.tenant_id = $1 AND a.id = ANY($2) AND a.deleted_at IS NULL \
             ORDER BY a.updated_at DESC, a.id DESC",
        )
        .bind(tenant_id)
        .bind(ids)
        .fetch_all(&self.db)
        .await
    }

    /// 認可済み id 集合を名前・表示名・説明で絞り、本人の所有を先に・更新日降順で `limit` 件返す。
    ///
    /// 絞り込み（語ごとの部分一致の AND・大文字小文字を区別しない）と件数の上限は SQL で効かせる
    /// （取ってから捨てない）。`interactive_only` は対話トリガを持つもの（図のボタン・チャットから
    /// 起動するもの）だけに絞る。id は認可済み前提で、tenant と kind で二重に絞る。
    /// `owned` は `ids` のうち本人が所有するもの。共有されただけのものは他人が説明を自由に書けるので、
    /// 更新日で上位を取られない（説明のスクワッティング）よう、所有分を先に並べて印を付ける。
    pub async fn search(
        &self,
        tenant_id: &str,
        ids: &[Uuid],
        owned: &[Uuid],
        query: &str,
        interactive_only: bool,
        limit: i64,
    ) -> Result<Vec<WorkflowSearchHit>, sqlx::Error> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let patterns = like_patterns(query);
        sqlx::query_as(
            "SELECT a.id, a.name, \
                    v.body->>'display_name' AS display_name, \
                    v.body->>'description' AS description, \
                    COALESCE( \
                      (SELECT array_agg(t->>'kind') \
                       FROM jsonb_array_elements(v.body->'triggers') AS t), \
                      '{}') AS trigger_kinds, \
                    v.body->'input_schema' AS input_schema, \
                    COALESCE(r.status, 'none') AS enabled_status, \
                    (a.id = ANY($7)) AS owned, \
                    a.updated_at \
             FROM artifact a \
             JOIN artifact_version v \
               ON v.tenant_id = a.tenant_id AND v.artifact_id = a.id \
              AND v.version = a.current_version \
             LEFT JOIN workflow_registration r \
               ON r.tenant_id = a.tenant_id AND r.workflow_id = a.id \
             WHERE a.tenant_id = $1 AND a.id = ANY($2) AND a.kind = $3 \
               AND a.deleted_at IS NULL \
               AND NOT EXISTS ( \
                 SELECT 1 FROM unnest($4::text[]) AS p(pat) \
                 WHERE NOT (a.name ILIKE p.pat ESCAPE '\\' \
                         OR COALESCE(v.body->>'display_name', '') ILIKE p.pat ESCAPE '\\' \
                         OR COALESCE(v.body->>'description', '') ILIKE p.pat ESCAPE '\\')) \
               AND (NOT $5 OR EXISTS ( \
                 SELECT 1 FROM jsonb_array_elements(v.body->'triggers') AS t \
                 WHERE t->>'kind' = 'interactive')) \
             ORDER BY (a.id = ANY($7)) DESC, a.updated_at DESC, a.id DESC \
             LIMIT $6",
        )
        .bind(tenant_id)
        .bind(ids)
        .bind(artifact::ArtifactKind::Workflow.as_str())
        .bind(&patterns)
        .bind(interactive_only)
        .bind(limit.clamp(1, 100))
        .bind(owned)
        .fetch_all(&self.db)
        .await
    }
}

impl WorkflowSummaryStore {
    /// 認可済み id 集合のワークフローごとに、`since` 以降に終わった実行の件数を数える
    /// （期間内に実行が終わったワークフローだけを返す）。
    ///
    /// 呼び出し側は返ったワークフローだけを権限で確かめ直してから合計する（post-filter）。
    /// id は認可済み前提で、tenant・org で二重に絞り、削除済みのワークフローは数えない。
    /// `ids` が `None` のときは id で絞らない（可読集合が大きすぎるときの縮退。post-filter 必須）。
    /// 集計の基準は終了時刻（`finished_at`・終端遷移で必ず入る）で、終了時刻の索引
    /// （`workflow_run_gc_idx`）に乗る。
    pub async fn run_counts(
        &self,
        tenant_id: &str,
        org: &str,
        ids: Option<&[Uuid]>,
        since: DateTime<Utc>,
    ) -> Result<Vec<WorkflowRunCount>, sqlx::Error> {
        if ids.is_some_and(<[Uuid]>::is_empty) {
            return Ok(Vec::new());
        }
        sqlx::query_as(
            "SELECT r.workflow_id, \
                    count(*) FILTER (WHERE r.status = 'succeeded') AS succeeded, \
                    count(*) FILTER (WHERE r.status = 'failed') AS failed \
             FROM workflow_run r \
             JOIN artifact a \
               ON a.tenant_id = r.tenant_id AND a.id = r.workflow_id \
              AND a.org = $2 AND a.deleted_at IS NULL \
             WHERE r.tenant_id = $1 AND ($3::uuid[] IS NULL OR r.workflow_id = ANY($3)) \
               AND r.status IN ('succeeded', 'failed', 'cancelled') AND r.finished_at >= $4 \
             GROUP BY r.workflow_id",
        )
        .bind(tenant_id)
        .bind(org)
        .bind(ids)
        .bind(since)
        .fetch_all(&self.db)
        .await
    }

    /// 認可済み id 集合のワークフローで `since` 以降に失敗した実行を、終了の新しい順に
    /// `limit` 件返す。削除済みのワークフローの実行は出さない。
    pub async fn recent_failures(
        &self,
        tenant_id: &str,
        ids: &[Uuid],
        since: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<FailedRun>, sqlx::Error> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as(
            "SELECT r.run_id, r.workflow_id, a.name AS workflow_name, \
                    v.body->>'display_name' AS display_name, \
                    left(r.fail_reason, 200) AS fail_reason, r.finished_at \
             FROM workflow_run r \
             JOIN artifact a \
               ON a.tenant_id = r.tenant_id AND a.id = r.workflow_id AND a.deleted_at IS NULL \
             JOIN artifact_version v \
               ON v.tenant_id = a.tenant_id AND v.artifact_id = a.id \
              AND v.version = a.current_version \
             WHERE r.tenant_id = $1 AND r.workflow_id = ANY($2) AND r.status = 'failed' \
               AND r.finished_at >= $3 \
             ORDER BY r.finished_at DESC, r.run_id DESC \
             LIMIT $4",
        )
        .bind(tenant_id)
        .bind(ids)
        .bind(since)
        .bind(limit.clamp(1, 20))
        .fetch_all(&self.db)
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::like_patterns;

    #[test]
    fn like_patterns_split_terms_and_escape_wildcards() {
        assert!(like_patterns("").is_empty());
        assert!(like_patterns("   ").is_empty());
        assert_eq!(like_patterns("経費 精算"), vec!["%経費%", "%精算%"]);
        // 利用者の % _ \ はワイルドカードにしない（「全件一致」を作らせない）。
        assert_eq!(like_patterns("100%"), vec!["%100\\%%"]);
        assert_eq!(like_patterns("a_b"), vec!["%a\\_b%"]);
        assert_eq!(like_patterns("c\\d"), vec!["%c\\\\d%"]);
    }
}
