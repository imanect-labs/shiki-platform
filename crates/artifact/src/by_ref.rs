//! 本文の参照で絞った一覧（例: あるテーブルの保存ビュー・Task 15.5）。`store.rs` から分離。
//!
//! 本文の参照キーの一致まで SQL で絞った候補（テナント・組織・種類・新しい順）を、「自分が owner」か OpenFGA の
//! viewer の判定（BatchCheck）で絞る（全件取得→フィルタをしない）。候補は少ないので ListObjects を引かない
//! （ListObjects の上限で、共有された分が黙って欠けない・BUG-149・[`crate::visible`]）。
//! tenant/org の突き合わせは一覧と同じ二重防御（候補の SQL と OpenFGA の名前空間）。

use authz::AuthContext;
use uuid::Uuid;

use crate::model::{Artifact, ArtifactKind};
use crate::store::{ArtifactRow, ArtifactStore};
use crate::visible::{in_order, Candidate};
use crate::{map_db, ArtifactError};

/// 本文で絞る候補の上限（これより多く当たったら新しい順に打ち切る）。
const MAX_CANDIDATES: i64 = 1_000;

impl ArtifactStore {
    /// `kind` の現行版の本文で `body[body_key] == body_value`（文字列）のもののうち、自分が owner か
    /// viewer のもの（更新日降順・上限 `limit`）。`body_key` はトップレベルのキー名（`[a-z_]+`）。
    pub async fn list_visible_with_body_ref(
        &self,
        ctx: &AuthContext,
        kind: ArtifactKind,
        body_key: &str,
        body_value: &str,
        limit: i64,
    ) -> Result<Vec<Artifact>, ArtifactError> {
        if body_key.is_empty() || !body_key.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            return Err(ArtifactError::Invalid("本文の参照キーが不正です".into()));
        }
        let limit = limit.clamp(1, 200);
        let candidates: Vec<Candidate> = sqlx::query_as(
            "SELECT a.id, a.owner, a.updated_at \
             FROM artifact a \
             JOIN artifact_version v \
               ON v.tenant_id = a.tenant_id AND v.artifact_id = a.id AND v.version = a.current_version \
             WHERE a.tenant_id = $1 AND a.org = $2 AND a.kind = $3 AND a.deleted_at IS NULL \
               AND v.body->>$4 = $5 \
             ORDER BY a.updated_at DESC, a.id DESC LIMIT $6",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(kind.as_str())
        .bind(body_key)
        .bind(body_value)
        .bind(MAX_CANDIDATES)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let ids = self.pick_candidates(ctx, &candidates, limit).await?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<ArtifactRow> = sqlx::query_as(
            "SELECT id, kind, name, owner, current_version, created_at, updated_at \
             FROM artifact \
             WHERE tenant_id = $1 AND org = $2 AND id = ANY($3) AND deleted_at IS NULL",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&ids)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let items = rows
            .into_iter()
            .map(ArtifactRow::into_artifact)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(in_order(&ids, items, |a| a.id))
    }

    /// `kind` の現行版の本文に `id`（UUID・大文字小文字を区別しない）が書かれているもののうち、自分が owner か
    /// viewer のもの（更新日降順・上限 `limit`）と、その現行版の本文。
    ///
    /// テーブルの状態の進め方を変える前に、そのテーブルを使っている保存済みのワークフローを探す（GAP-44）。
    /// 本文のどこに書かれているか（どのブロックか）は呼び出し側が本文を読んで決める。見えないものは返さない。
    pub async fn list_visible_mentioning(
        &self,
        ctx: &AuthContext,
        kind: ArtifactKind,
        id: Uuid,
        limit: i64,
    ) -> Result<Vec<(Artifact, serde_json::Value)>, ArtifactError> {
        let limit = limit.clamp(1, 200);
        // UUID の文字（16 進とハイフン）は LIKE の特殊文字を含まない。
        let needle = format!("%{}%", id.hyphenated());
        // 本文の全文の照合（BUG-157）は 1 回だけにする: 候補は id・持ち主・並びのキーだけを引き、本文は選んだ分だけ読む。
        let candidates: Vec<Candidate> = sqlx::query_as(
            "SELECT a.id, a.owner, a.updated_at \
             FROM artifact a \
             JOIN artifact_version v \
               ON v.tenant_id = a.tenant_id AND v.artifact_id = a.id AND v.version = a.current_version \
             WHERE a.tenant_id = $1 AND a.org = $2 AND a.kind = $3 AND a.deleted_at IS NULL \
               AND v.body::text ILIKE $4 \
             ORDER BY a.updated_at DESC, a.id DESC LIMIT $5",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(kind.as_str())
        .bind(&needle)
        .bind(MAX_CANDIDATES)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let ids = self.pick_candidates(ctx, &candidates, limit).await?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        #[derive(sqlx::FromRow)]
        struct MentionRow {
            #[sqlx(flatten)]
            meta: ArtifactRow,
            body: sqlx::types::Json<serde_json::Value>,
        }
        let rows: Vec<MentionRow> = sqlx::query_as(
            "SELECT a.id, a.kind, a.name, a.owner, a.current_version, a.created_at, a.updated_at, \
                    v.body \
             FROM artifact a \
             JOIN artifact_version v \
               ON v.tenant_id = a.tenant_id AND v.artifact_id = a.id AND v.version = a.current_version \
             WHERE a.tenant_id = $1 AND a.org = $2 AND a.id = ANY($3) AND a.deleted_at IS NULL",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&ids)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let items = rows
            .into_iter()
            .map(|r| r.meta.into_artifact().map(|a| (a, r.body.0)))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(in_order(&ids, items, |(a, _)| a.id))
    }

    /// 候補（新しい順）から、見えるものを先頭から `limit` 件選ぶ。
    async fn pick_candidates(
        &self,
        ctx: &AuthContext,
        candidates: &[Candidate],
        limit: i64,
    ) -> Result<Vec<Uuid>, ArtifactError> {
        if i64::try_from(candidates.len()).unwrap_or(i64::MAX) >= MAX_CANDIDATES {
            tracing::warn!(
                limit = MAX_CANDIDATES,
                "本文の参照で絞った候補が上限に届いた（新しい順に打ち切る）"
            );
        }
        let limit = usize::try_from(limit).unwrap_or(1);
        Ok(self.pick_visible(ctx, candidates, limit).await?.ids)
    }
}
