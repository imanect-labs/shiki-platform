//! 監査ログの閲覧（#634・Task 8.2）。
//!
//! 書き込み（[`crate::audit`]）と分け、検索だけを持つ。範囲は常に `AuthContext` の
//! tenant_id ＋ org で、呼び出し側から範囲を受け取らない（アンビエント権限の禁止）。
//! 誰が読めるか（`organization#audit_viewer`）はルートのポリシーが一律に確かめるので、
//! ここは判定しない。
//!
//! 返すのは監査の行そのもの（ID・判定・metadata）と、操作した人の表示名（名簿）だけ。
//! ファイル名や本文は解決しない（PIT-65: 監査担当者でも中身は通常の ReBAC でしか読めない）。

use authz::AuthContext;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Postgres, QueryBuilder, Row};
use utoipa::ToSchema;

use crate::error::StorageError;

/// 認可判定（監査ログの `decision` 列と同じ閉じた集合）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AuditDecision {
    Allow,
    Deny,
}

impl AuditDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditDecision::Allow => "allow",
            AuditDecision::Deny => "deny",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "allow" => Some(AuditDecision::Allow),
            "deny" => Some(AuditDecision::Deny),
            _ => None,
        }
    }
}

/// 検索条件。期間は `[from, to)`。`None` の条件は絞らない。
#[derive(Debug, Clone, Default)]
pub struct AuditFilter {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// 操作した人（ユーザー ID か、名簿のメールアドレス）。
    pub actor: Option<String>,
    /// 操作の種類。末尾が `.` なら前方一致（`file.` で file 系すべて）、それ以外は完全一致。
    pub action: Option<String>,
    pub decision: Option<AuditDecision>,
    pub object_type: Option<String>,
    pub object_id: Option<String>,
    /// 引用されたチャンク ID（`rag.search` の `metadata.cited_chunk_ids` に含む行）。
    pub chunk_id: Option<String>,
    pub trace_id: Option<String>,
}

/// keyset ページングの位置（直前のページの最後の行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditCursor {
    pub created_at: DateTime<Utc>,
    pub id: i64,
}

impl AuditCursor {
    /// `"<created_at のマイクロ秒>.<id>"`（API にそのまま渡す不透明な文字列）。
    pub fn encode(&self) -> String {
        format!("{}.{}", self.created_at.timestamp_micros(), self.id)
    }

    /// [`AuditCursor::encode`] の逆。形が違えば `None`。
    pub fn decode(s: &str) -> Option<Self> {
        let (ts, id) = s.split_once('.')?;
        Some(AuditCursor {
            created_at: DateTime::from_timestamp_micros(ts.parse().ok()?)?,
            id: id.parse().ok()?,
        })
    }
}

/// 監査ログの 1 行。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AuditEvent {
    pub id: i64,
    pub created_at: DateTime<Utc>,
    /// 操作した人（ユーザー ID・システム主体）。
    pub actor: String,
    /// 名簿の表示名（名簿に無い主体は `None`）。
    pub actor_name: Option<String>,
    pub action: String,
    pub object_type: String,
    pub object_id: String,
    pub decision: AuditDecision,
    pub trace_id: Option<String>,
    /// 操作ごとの付帯情報（引用チャンク ID・拒否されたファイル ID など）。
    #[schema(value_type = Object)]
    pub metadata: Value,
    /// ハッシュチェーンに連結された行か（実データを変える操作だけ・PIT-12）。
    pub chained: bool,
}

/// 1 ページの上限。
pub const MAX_PAGE: i64 = 500;

/// 監査ログの閲覧窓口。
#[derive(Clone)]
pub struct AuditViewer {
    db: PgPool,
}

impl AuditViewer {
    pub fn new(db: PgPool) -> Self {
        AuditViewer { db }
    }

    /// 条件に合う行を新しい順に最大 `limit` 件返す（`after` はひとつ前のページの最後の行）。
    pub async fn search(
        &self,
        ctx: &AuthContext,
        filter: &AuditFilter,
        after: Option<AuditCursor>,
        limit: i64,
    ) -> Result<Vec<AuditEvent>, StorageError> {
        // 操作した人はメールでも引けるが、検索の SQL には 1 つのユーザー ID として渡す
        // （`IN (… UNION …)` にすると actor 索引が使われず、期間を全部読んでしまう）。
        let actor = match &filter.actor {
            None => None,
            Some(a) => Some(self.resolve_actor(ctx, a).await?),
        };
        let mut q = build_query(
            ctx,
            filter,
            actor.as_deref(),
            after,
            limit.clamp(1, MAX_PAGE),
        );
        let rows = q.build().fetch_all(&self.db).await?;
        rows.iter()
            .map(|r| {
                Ok(AuditEvent {
                    id: r.try_get("id")?,
                    created_at: r.try_get("created_at")?,
                    actor: r.try_get("actor")?,
                    actor_name: r.try_get("actor_name")?,
                    action: r.try_get("action")?,
                    object_type: r.try_get("object_type")?,
                    object_id: r.try_get("object_id")?,
                    decision: {
                        let d: String = r.try_get("decision")?;
                        // 列の CHECK と同じ集合なので外れることはない（外れたら壊れた行として扱う）。
                        AuditDecision::parse(&d).ok_or_else(|| {
                            sqlx::Error::Decode(format!("unknown audit decision: {d}").into())
                        })?
                    },
                    trace_id: r.try_get("trace_id")?,
                    metadata: r.try_get("metadata")?,
                    chained: r.try_get("chained")?,
                })
            })
            .collect()
    }
}

impl AuditViewer {
    /// 名簿のメールアドレス（自組織・大文字小文字を区別しない）ならユーザー ID に、そうでなければ
    /// 入力をそのままユーザー ID として扱う。
    async fn resolve_actor(&self, ctx: &AuthContext, actor: &str) -> Result<String, StorageError> {
        if !actor.contains('@') {
            return Ok(actor.to_string());
        }
        let id: Option<String> = sqlx::query_scalar(
            "SELECT user_id FROM directory_user \
             WHERE tenant_id = $1 AND org = $2 AND lower(email) = lower($3) LIMIT 1",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(actor)
        .fetch_optional(&self.db)
        .await?;
        Ok(id.unwrap_or_else(|| actor.to_string()))
    }
}

/// LIKE の特殊文字（`%` `_` `\`）を打ち消す。
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// 検索の SQL を組む（値はすべてバインド）。`actor` は呼び出し側で解決済みのユーザー ID。
///
/// 引用チャンクで引くときは、GIN 索引で該当行を先に絞ってから並べる（jsonb の `?` は件数の
/// 見積もりが固定値なので、広い期間だとプランナが時刻索引を全部歩いてしまう）。
fn build_query<'a>(
    ctx: &'a AuthContext,
    f: &'a AuditFilter,
    actor: Option<&'a str>,
    after: Option<AuditCursor>,
    limit: i64,
) -> QueryBuilder<'a, Postgres> {
    let mut q = QueryBuilder::new("");
    let source = if let Some(chunk) = &f.chunk_id {
        q.push(
            "WITH cited AS MATERIALIZED (SELECT * FROM audit_log \
             WHERE action = 'rag.search' AND metadata -> 'cited_chunk_ids' ? ",
        )
        .push_bind(chunk)
        .push(" AND tenant_id = ")
        .push_bind(&ctx.tenant_id)
        .push(" AND org = ")
        .push_bind(&ctx.org)
        .push(") ");
        "cited"
    } else {
        "audit_log"
    };
    q.push(
        "SELECT a.id, a.created_at, a.actor, du.display_name AS actor_name, a.action, \
         a.object_type, a.object_id, a.decision, a.trace_id, a.metadata, a.chained FROM ",
    )
    .push(source)
    .push(
        " a LEFT JOIN directory_user du ON du.tenant_id = a.tenant_id AND du.user_id = a.actor \
         WHERE a.tenant_id = ",
    )
    .push_bind(&ctx.tenant_id)
    .push(" AND a.org = ")
    .push_bind(&ctx.org)
    .push(" AND a.created_at >= ")
    .push_bind(f.from)
    .push(" AND a.created_at < ")
    .push_bind(f.to);
    if let Some(actor) = actor {
        q.push(" AND a.actor = ").push_bind(actor);
    }
    if let Some(action) = &f.action {
        if action.ends_with('.') {
            q.push(" AND a.action LIKE ")
                .push_bind(format!("{}%", escape_like(action)));
        } else {
            q.push(" AND a.action = ").push_bind(action);
        }
    }
    if let Some(decision) = f.decision {
        q.push(" AND a.decision = ").push_bind(decision.as_str());
    }
    if let Some(t) = &f.object_type {
        q.push(" AND a.object_type = ").push_bind(t);
    }
    if let Some(id) = &f.object_id {
        q.push(" AND a.object_id = ").push_bind(id);
    }
    if let Some(trace) = &f.trace_id {
        q.push(" AND a.trace_id = ").push_bind(trace);
    }
    if let Some(c) = after {
        q.push(" AND (a.created_at, a.id) < (")
            .push_bind(c.created_at)
            .push(", ")
            .push_bind(c.id)
            .push(")");
    }
    q.push(" ORDER BY a.created_at DESC, a.id DESC LIMIT ")
        .push_bind(limit);
    q
}

#[cfg(test)]
mod tests {
    use super::*;
    use authz::{Principal, PrincipalKind};

    fn ctx() -> AuthContext {
        AuthContext::new(
            Principal {
                kind: PrincipalKind::User,
                id: "alice".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: None,
            },
            "acme".into(),
            "t1".into(),
        )
    }

    #[test]
    fn cursor_roundtrip() {
        let c = AuditCursor {
            created_at: DateTime::from_timestamp_micros(1_792_000_000_123_456).unwrap_or_default(),
            id: 42,
        };
        assert_eq!(AuditCursor::decode(&c.encode()), Some(c));
    }

    #[test]
    fn cursor_rejects_garbage() {
        assert_eq!(AuditCursor::decode(""), None);
        assert_eq!(AuditCursor::decode("abc.1"), None);
        assert_eq!(AuditCursor::decode("1"), None);
        assert_eq!(AuditCursor::decode("1.x"), None);
    }

    #[test]
    fn like_special_chars_are_escaped() {
        assert_eq!(escape_like("file."), "file.");
        assert_eq!(escape_like("a%b_c\\"), "a\\%b\\_c\\\\");
    }

    #[test]
    fn query_is_scoped_and_binds_every_value() {
        let c = ctx();
        let f = AuditFilter {
            actor: Some("bob@example.com".into()),
            action: Some("file.".into()),
            decision: Some(AuditDecision::Deny),
            object_type: Some("file".into()),
            object_id: Some("f1".into()),
            chunk_id: Some("c1".into()),
            trace_id: Some("t".into()),
            ..AuditFilter::default()
        };
        let after = Some(AuditCursor {
            created_at: Utc::now(),
            id: 7,
        });
        let sql = build_query(&c, &f, Some("u-bob"), after, 50).into_sql();
        assert!(sql.starts_with("WITH cited AS MATERIALIZED"));
        assert!(sql.contains("cited_chunk_ids' ? $1 AND tenant_id = $2 AND org = $3"));
        assert!(sql.contains("FROM cited a"));
        assert!(sql.contains("a.tenant_id = $4 AND a.org = $5"));
        assert!(sql.contains("a.actor = $8"));
        assert!(sql.contains("a.action LIKE"));
        assert!(sql.contains("(a.created_at, a.id) <"));
        assert!(sql.ends_with("ORDER BY a.created_at DESC, a.id DESC LIMIT $16"));
        // 利用者の入力は SQL に埋め込まない。
        assert!(!sql.contains("bob@example.com"));
        assert!(!sql.contains("f1"));
    }

    #[test]
    fn exact_action_does_not_use_like() {
        let c = ctx();
        let f = AuditFilter {
            action: Some("file.delete".into()),
            ..AuditFilter::default()
        };
        let sql = build_query(&c, &f, None, None, 10).into_sql();
        assert!(sql.contains("FROM audit_log a"));
        assert!(sql.contains("a.action = $5"));
        assert!(!sql.contains("LIKE"));
    }
}
