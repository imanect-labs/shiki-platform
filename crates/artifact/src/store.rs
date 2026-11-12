//! `ArtifactStore` — アーティファクトの単一チョークポイント（`&AuthContext` 経由・PgPool 内包）。
//!
//! 作成・不変バージョン追記・取得・名前解決・一覧・論理削除。共有は [`share`](crate::share)。

use std::sync::Arc;
use storage::audit::AuditAction;

use authz::{AuthContext, AuthzClient, Consistency, Relation};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::types::Json;
use sqlx::PgPool;
use storage::audit::{AuditEntry, AuditRecorder, Decision};
use storage::tuple_commit::{commit_tx_with_tuples, TupleCommitError, TupleKey};
use uuid::Uuid;

use crate::model::{Artifact, ArtifactKind, ConflictReason};
use crate::{map_db, ArtifactError};

/// 本文 JSON の上限（防御的上限。IR の 1MB 上限（ir.md V7）は保存時検証側が別途課す）。
const MAX_BODY_BYTES: usize = 1024 * 1024;
/// 参照名の上限長。
const MAX_NAME_LEN: usize = 128;

/// 新規アーティファクトの入力。
#[derive(Debug, Clone)]
pub struct NewArtifact {
    pub kind: ArtifactKind,
    pub name: String,
    pub body: serde_json::Value,
}

/// 一覧の続きの位置（並びのキー `(updated_at, id)` の最後の値・keyset）。
pub type ArtifactPosition = (DateTime<Utc>, Uuid);

/// 一覧の 1 ページ（更新の新しい順）。`next` が `Some` なら続きがある（共有された一覧では `items` が空でも続きが
/// あることがある・[`ArtifactStore::list_shared_page`]）。
#[derive(Debug)]
pub struct ArtifactPage {
    pub items: Vec<Artifact>,
    pub next: Option<ArtifactPosition>,
}

/// skill カタログ用の要約 1 件（name + description のみ・本文は載せない・#344 Task 10.11）。
///
/// skill ツールの動的 description（モデルに見せる一覧）用。instructions 等の本文を
/// 引かないための専用射影（全件取得→フィルタをしない）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SkillSummary {
    pub id: Uuid,
    pub name: String,
    pub current_version: i64,
    /// current_version の body `description`（保存時検証済み・最大 1024 字）。
    pub description: String,
    /// current_version の body `command`（スラッシュコマンド宣言・#387）。
    /// body 全体は読まない（カタログは name+description+command だけで足りる）。
    pub command: Option<serde_json::Value>,
}

/// artifact 行の列（[`ArtifactRow`] が読む列・一覧と取得の SELECT はすべてこれを使い、列を足すときはここだけを直す）。
/// 引数に表の別名（`"a"`）を渡すと、列に別名を付ける。
macro_rules! artifact_columns {
    () => {
        "id, kind, name, owner, current_version, published_version, previous_published_version, \
         published_at, created_at, updated_at"
    };
    ($a:literal) => {
        concat!(
            $a,
            ".id, ",
            $a,
            ".kind, ",
            $a,
            ".name, ",
            $a,
            ".owner, ",
            $a,
            ".current_version, ",
            $a,
            ".published_version, ",
            $a,
            ".previous_published_version, ",
            $a,
            ".published_at, ",
            $a,
            ".created_at, ",
            $a,
            ".updated_at"
        )
    };
}
pub(crate) use artifact_columns;

/// artifact 行。
#[derive(sqlx::FromRow)]
pub(crate) struct ArtifactRow {
    id: Uuid,
    kind: String,
    name: String,
    owner: String,
    current_version: i64,
    published_version: Option<i64>,
    previous_published_version: Option<i64>,
    published_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl ArtifactRow {
    pub(crate) fn into_artifact(self) -> Result<Artifact, ArtifactError> {
        Ok(Artifact {
            id: self.id,
            kind: ArtifactKind::parse(&self.kind)
                .ok_or_else(|| ArtifactError::Internal(format!("bad kind: {}", self.kind)))?,
            name: self.name,
            owner: self.owner,
            current_version: self.current_version,
            published_version: self.published_version,
            previous_published_version: self.previous_published_version,
            published_at: self.published_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

/// アーティファクトのデータチョークポイント。
#[derive(Clone)]
pub struct ArtifactStore {
    pub(crate) db: PgPool,
    pub(crate) authz: Arc<dyn AuthzClient>,
    pub(crate) audit: AuditRecorder,
}

impl ArtifactStore {
    pub fn new(db: PgPool, authz: Arc<dyn AuthzClient>) -> Self {
        let audit = AuditRecorder::new(db.clone());
        ArtifactStore { db, authz, audit }
    }

    /// アーティファクトを作成する（version 1 の本文込み・作成者を owner タプルで付与）。
    pub async fn create(
        &self,
        ctx: &AuthContext,
        input: NewArtifact,
        trace_id: Option<&str>,
    ) -> Result<Artifact, ArtifactError> {
        let name = validate_name(&input.name)?;
        validate_body(&input.body)?;

        let mut tx = self.db.begin().await.map_err(map_db)?;
        // 下書きと公開を分ける種類は、まだ公開していない（published_version は null・D-16）。
        let row: ArtifactRow = sqlx::query_as(concat!(
            "INSERT INTO artifact (tenant_id, org, kind, name, owner, current_version) \
             VALUES ($1, $2, $3, $4, $5, 1) RETURNING ",
            artifact_columns!()
        ))
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(input.kind.as_str())
        .bind(name)
        .bind(&ctx.principal().id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                ArtifactError::Conflict(ConflictReason::NameTaken { kind: input.kind })
            }
            _ => map_db(e),
        })?;
        sqlx::query(
            "INSERT INTO artifact_version (tenant_id, artifact_id, version, body, created_by) \
             VALUES ($1, $2, 1, $3, $4)",
        )
        .bind(ctx.tenant_id())
        .bind(row.id)
        .bind(Json(&input.body))
        .bind(&ctx.principal().id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        // 作成の監査も同じ Tx に入れる（storage の作成と同じ形）。下の区間は呼ぶ側を捨てても commit まで走るので、
        // 監査を commit の後に別の Tx で書くと、その前に捨てられたとき監査の無い作成が残る。監査に失敗すれば
        // タプルを書く前に巻き戻る。チェーンはしない（前の `record` と同じ・つなぐとチェーンのロックを OpenFGA の
        // 往復の間も持つことになる）。
        let id = row.id;
        storage::audit::record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: AuditAction::ArtifactCreate,
                object_type: "artifact",
                object_id: &id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: json!({ "kind": input.kind, "name": name }),
            },
            storage::audit::Chain::No,
        )
        .await
        .map_err(|e| ArtifactError::Internal(format!("audit: {e}")))?;

        // 作成者を owner に（FGA）してから commit する（storage の作成と同じ順・BUG-134・7p-10）。commit の後に書くと、
        // その間で呼ぶ側のフューチャが捨てられたとき owner の無い行が残り、誰も読めないのに名前を塞ぎ続ける。区間は
        // 別のタスクで最後まで走り、書き込みに失敗すれば commit せずに巻き戻し、commit に失敗すれば書いたタプルを外す。
        let owner = TupleKey::new(
            ctx.subject(),
            Relation::Owner,
            ctx.ns().artifact(&id.to_string()),
        );
        commit_tx_with_tuples(Arc::clone(&self.authz), tx, vec![owner])
            .await
            .map_err(|e| match e {
                TupleCommitError::Write(e) => ArtifactError::Internal(format!("owner tuple: {e}")),
                TupleCommitError::Finish(e) => map_db(e),
                TupleCommitError::Aborted(reason) => ArtifactError::Internal(reason),
            })?;
        row.into_artifact()
    }

    /// 名前でメタデータを解決する（viewer 権限・保存時検証 V4 / ワークフロー起動が使う）。
    pub async fn get_by_name(
        &self,
        ctx: &AuthContext,
        kind: ArtifactKind,
        name: &str,
        trace_id: Option<&str>,
    ) -> Result<Artifact, ArtifactError> {
        let id: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM artifact \
             WHERE tenant_id = $1 AND kind = $2 AND name = $3 AND deleted_at IS NULL",
        )
        .bind(ctx.tenant_id())
        .bind(kind.as_str())
        .bind(name)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let id = id.ok_or(ArtifactError::NotFound)?;
        self.get(ctx, id, trace_id).await
    }

    /// 本人が `since` 以降に作った、消していない同名のアーティファクトを引く（作成の冪等・R1・7p-10）。
    ///
    /// 作成が名前の重なり（NameTaken）で断られたとき、それが同じ会話の run の中で本人が作ったものかを確かめるのに
    /// 使う（`since` は run の作られた時刻）。SQL は (テナント・種類・名前・持ち主＝本人・作成時刻・生存) で 1 行を
    /// 引き、続けて [`Self::get`] で ReBAC の viewer を確かめる（読めなければ `None`・道具に認可を散らさない）。
    /// ほかの人の同名・`since` より前に作ったもの・消したものも `None`。
    pub async fn find_own_created_since(
        &self,
        ctx: &AuthContext,
        kind: ArtifactKind,
        name: &str,
        since: DateTime<Utc>,
        trace_id: Option<&str>,
    ) -> Result<Option<Artifact>, ArtifactError> {
        let id: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM artifact \
             WHERE tenant_id = $1 AND kind = $2 AND name = $3 AND owner = $4 \
               AND created_at >= $5 AND deleted_at IS NULL",
        )
        .bind(ctx.tenant_id())
        .bind(kind.as_str())
        .bind(name.trim())
        .bind(&ctx.principal().id)
        .bind(since)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let Some(id) = id else {
            return Ok(None);
        };
        match self.get(ctx, id, trace_id).await {
            Ok(found) => Ok(Some(found)),
            Err(ArtifactError::NotFound | ArtifactError::Forbidden) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// 自分が owner のアーティファクトの 1 ページ（kind 絞り込み・更新日降順・keyset ページング）。
    ///
    /// `before` は前のページの [`ArtifactPage::next`]。1 件多く引いて、後ろにまだあるときだけ続きの位置（返した最後の
    /// 1 件）を返す（ちょうど `limit` 件で終わる一覧に、空の続きを読ませない・BUG-186）。
    pub async fn list_mine_page(
        &self,
        ctx: &AuthContext,
        kind: Option<ArtifactKind>,
        before: Option<ArtifactPosition>,
        limit: i64,
    ) -> Result<ArtifactPage, ArtifactError> {
        let limit = limit.clamp(1, 100);
        let (before_at, before_id) = match before {
            Some((at, id)) => (Some(at), Some(id)),
            None => (None, None),
        };
        let rows: Vec<ArtifactRow> = sqlx::query_as(concat!(
            "SELECT ",
            artifact_columns!(),
            " FROM artifact \
             WHERE tenant_id = $1 AND org = $2 AND owner = $3 AND deleted_at IS NULL \
               AND ($4::text IS NULL OR kind = $4) \
               AND ($5::timestamptz IS NULL OR (updated_at, id) < ($5::timestamptz, $6)) \
             ORDER BY updated_at DESC, id DESC LIMIT $7"
        ))
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(&ctx.principal().id)
        .bind(kind.map(ArtifactKind::as_str))
        .bind(before_at)
        .bind(before_id)
        .bind(limit + 1)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let mut items = rows
            .into_iter()
            .map(ArtifactRow::into_artifact)
            .collect::<Result<Vec<_>, _>>()?;
        let more = items.len() > usize::try_from(limit).unwrap_or(usize::MAX);
        items.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        let next = if more {
            items.last().map(|a| (a.updated_at, a.id))
        } else {
            None
        };
        Ok(ArtifactPage { items, next })
    }

    /// 自分が owner の skill の要約一覧（name + description のみ・skill ツールのカタログ用・#344）。
    ///
    /// 掲載範囲を「本人 owner」に限るのは description スクワッティング防御の一部
    /// （共有されただけの skill を他人のカタログへ自動掲載しない。掲載＝明示的な人間の行為）。
    pub async fn list_my_skill_summaries(
        &self,
        ctx: &AuthContext,
        limit: i64,
    ) -> Result<Vec<SkillSummary>, ArtifactError> {
        let limit = limit.clamp(1, 200);
        let rows: Vec<SkillSummary> = sqlx::query_as(
            "SELECT a.id, a.name, a.current_version, \
                    coalesce(v.body->>'description', '') AS description, \
                    v.body->'command' AS command \
             FROM artifact a \
             JOIN artifact_version v \
               ON v.tenant_id = a.tenant_id AND v.artifact_id = a.id AND v.version = a.current_version \
             WHERE a.tenant_id = $1 AND a.org = $2 AND a.owner = $3 \
               AND a.kind = 'skill' AND a.deleted_at IS NULL \
             ORDER BY a.name LIMIT $4",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(&ctx.principal().id)
        .bind(limit)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows)
    }

    /// 論理削除する（owner 権限・バージョン履歴は保持・名前は再利用可能になる）。消したものの種類を返す。
    ///
    /// ワークフロー（kind=workflow）の論理削除には DB のトリガが掛かる（同じ文の Tx の中で、判定の決定キャッシュを
    /// 消し〔0127〕、自動実行をオフにしてきっかけを止め、動いている run に取り消しの印を付ける〔0130・BUG-128〕）。
    /// 委譲（FGA のタプル）の取り消しと run のドレインは、呼び出し側が commit の後に行う（API の削除）。
    pub async fn delete(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<ArtifactKind, ArtifactError> {
        self.require(
            ctx,
            id,
            Relation::Owner,
            AuditAction::ArtifactDelete,
            trace_id,
        )
        .await?;
        let kind: Option<String> = sqlx::query_scalar(
            "UPDATE artifact SET deleted_at = now(), updated_at = now() \
             WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL RETURNING kind",
        )
        .bind(ctx.tenant_id())
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let kind = kind.ok_or(ArtifactError::NotFound)?;
        let kind = ArtifactKind::parse(&kind)
            .ok_or_else(|| ArtifactError::Internal(format!("bad kind: {kind}")))?;
        self.record_audit(
            ctx,
            AuditAction::ArtifactDelete,
            &id.to_string(),
            trace_id,
            json!({}),
        )
        .await?;
        Ok(kind)
    }

    /// 生存行のメタを引く（認可済み前提の内部ヘルパ）。
    pub(crate) async fn fetch_live(
        &self,
        ctx: &AuthContext,
        id: Uuid,
    ) -> Result<Artifact, ArtifactError> {
        let row: Option<ArtifactRow> = sqlx::query_as(concat!(
            "SELECT ",
            artifact_columns!(),
            " FROM artifact WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL"
        ))
        .bind(ctx.tenant_id())
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        row.ok_or(ArtifactError::NotFound)?.into_artifact()
    }

    /// アーティファクトへの relation を確かめる（不足は監査 deny＋Forbidden）。保存の前に前の版を読んで比べる
    /// 呼び出し側（ミニアプリの保存・BUG-80）が、読む前に保存できる人かを確かめるのに使う。
    pub async fn require_relation(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        relation: Relation,
        action: AuditAction,
        trace_id: Option<&str>,
    ) -> Result<(), ArtifactError> {
        self.require(ctx, id, relation, action, trace_id)
            .await
            .map(|_| ())
    }

    /// アーティファクトへの relation があるか（監査に残さない・判定だけ）。下書きを読める人（editor）かを
    /// 決めるのに使う（D-16）。
    ///
    /// [`Self::require_relation`] と同じ問い方（同じ relation・`HigherConsistency`）で、拒否の監査を残さない。画面の
    /// 出し分けのための判定（試運転の下見の「試運転できるか」・D-197）はこれを呼び、実際に止める判定
    /// （`require_relation`）と問いを 2 か所に分けない（`require` もこの関数で判定する）。
    pub async fn allows(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        relation: Relation,
    ) -> Result<bool, ArtifactError> {
        let obj = ctx.ns().artifact(&id.to_string());
        self.authz
            .check(
                &ctx.subject(),
                relation,
                &obj,
                Consistency::HigherConsistency,
            )
            .await
            .map_err(|e| ArtifactError::Internal(e.to_string()))
    }

    /// アーティファクトへの relation を要求する（不足は監査 deny＋Forbidden・剥奪即時反映）。
    pub(crate) async fn require(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        relation: Relation,
        action: AuditAction,
        trace_id: Option<&str>,
    ) -> Result<authz::FgaObject, ArtifactError> {
        let obj = ctx.ns().artifact(&id.to_string());
        if !self.allows(ctx, id, relation).await? {
            let _ = self
                .audit
                .record(
                    ctx,
                    AuditEntry {
                        action,
                        object_type: "artifact",
                        object_id: &id.to_string(),
                        decision: Decision::Deny,
                        trace_id,
                        metadata: json!({ "relation": relation.as_str() }),
                    },
                )
                .await;
            return Err(ArtifactError::Forbidden);
        }
        Ok(obj)
    }

    pub(crate) async fn record_audit(
        &self,
        ctx: &AuthContext,
        action: AuditAction,
        object_id: &str,
        trace_id: Option<&str>,
        metadata: serde_json::Value,
    ) -> Result<(), ArtifactError> {
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action,
                    object_type: "artifact",
                    object_id,
                    decision: Decision::Allow,
                    trace_id,
                    metadata,
                },
            )
            .await
            .map_err(|e| ArtifactError::Internal(format!("audit: {e}")))
    }
}

/// 参照名を検証する（trim 済みを返す）。
fn validate_name(name: &str) -> Result<&str, ArtifactError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ArtifactError::Invalid("name が空です".into()));
    }
    // NUL 文字は Postgres の text に保存できない（DB の誤りの 500 にしない・検証 2-D-P3-1）。
    if name.contains('\0') {
        return Err(ArtifactError::Invalid(
            "name に NUL 文字（U+0000）は使えません".into(),
        ));
    }
    if name.len() > MAX_NAME_LEN {
        return Err(ArtifactError::Invalid(format!(
            "name が長すぎます（最大 {MAX_NAME_LEN} 文字）"
        )));
    }
    Ok(name)
}

/// 値の中（入れ子の配列・オブジェクトのキーと文字列）に NUL があるか。
fn contains_nul(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::String(s) => s.contains('\0'),
        serde_json::Value::Array(a) => a.iter().any(contains_nul),
        serde_json::Value::Object(m) => m.iter().any(|(k, v)| k.contains('\0') || contains_nul(v)),
        _ => false,
    }
}

/// 本文 JSON のサイズ上限を検証する。
pub(crate) fn validate_body(body: &serde_json::Value) -> Result<(), ArtifactError> {
    // serde_json の直列化長で近似（DB の jsonb サイズとオーダー一致で十分）。
    // jsonb は文字列の NUL（U+0000）を保存できない（DB の誤りの 500 にしない・検証 2-D-P3-1）。
    if contains_nul(body) {
        return Err(ArtifactError::Invalid(
            "body に NUL 文字（U+0000）は使えません".into(),
        ));
    }
    let size = serde_json::to_vec(body)
        .map_err(|e| ArtifactError::Invalid(format!("body: {e}")))?
        .len();
    if size > MAX_BODY_BYTES {
        return Err(ArtifactError::Invalid(format!(
            "body が大きすぎます（{size} bytes > {MAX_BODY_BYTES} bytes）"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_validation() {
        assert!(validate_name("  wf-1  ").is_ok_and(|n| n == "wf-1"));
        assert!(validate_name("   ").is_err());
        assert!(validate_name(&"x".repeat(129)).is_err());
        assert!(validate_name("a\0b").is_err());
        assert!(validate_body(&serde_json::json!({"x": "a\u{0}b"})).is_err());
        assert!(validate_body(&serde_json::json!({"x": "a\\u0000b"})).is_ok());
    }

    #[test]
    fn body_size_limit() {
        assert!(validate_body(&serde_json::json!({"a": 1})).is_ok());
        let big = serde_json::json!({ "blob": "x".repeat(MAX_BODY_BYTES) });
        assert!(validate_body(&big).is_err());
    }
}
