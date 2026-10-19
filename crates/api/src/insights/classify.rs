//! 会話の用途分類（`thread_insight`・#751）。
//!
//! 組織の設定（`classify_enabled`）がオンのときだけ動く。会話の最初のユーザー発話（最大 3 件・
//! 先頭 2,000 文字）を LLM に渡して用途ラベルを 1 つ選ばせ、**ラベルだけ**を保存する
//! （本文は保存しない・Langfuse へのプレビューにも本文を載せない）。
//!
//! - LLM は llm-gateway 経由（会計に載る・組織が許可したモデルだけ・単一チョークポイント）。
//!   主体は [`super::CLASSIFIER_PRINCIPAL`]（利用者の集計からは除く）。
//! - 分類し直すのは発話が 3 件に届くまで（分類に使う範囲が変わらなくなったら止める）。
//! - 1 回の実行で 1 組織あたり [`super::InsightsConfig::classify_batch`] 件まで。

use authz::{AuthContext, Principal, PrincipalKind};
use futures::StreamExt;
use llm_gateway::{
    GenerateRequest, GenerationRecord, LlmGateway, Message, ModelChoice, Role, StreamDelta, Usage,
    UsageSource,
};
use serde_json::json;
use sqlx::PgPool;
use storage::audit::{AuditEntry, AuditRecorder, Decision};
use uuid::Uuid;

use super::{CATEGORIES, CLASSIFIER_PRINCIPAL};

/// 分類に使うユーザー発話の件数。
const MESSAGES_FOR_CLASSIFY: i64 = 3;
/// LLM に渡す抜粋の上限（文字）。
const EXCERPT_CHARS: usize = 2000;

const SYSTEM_PROMPT: &str = "あなたは社内の AI 利用状況を集計するための分類器です。\
利用者が AI に頼んだ内容を読み、何のための利用かを次のラベルから 1 つだけ選んでください。\
出力はラベルのキー（英小文字）だけにし、説明や記号は付けないでください。\n\n\
writing: メール・提案書・報告書などの文書を書く・直す\n\
summarize: 議事録・資料・長文の要約\n\
research: 調査・情報収集・比較・説明を求める\n\
internal_search: 社内文書・規程・ナレッジを探す、社内の手続きを尋ねる\n\
data_analysis: 表・数値の集計や分析、表計算\n\
translation: 翻訳\n\
ideation: アイデア出し・壁打ち・相談\n\
coding: プログラミング・SQL・スクリプト\n\
other: 上のどれにも当てはまらない";

/// 分類の対象（未分類、または分類後に発話が増えた会話・発話が 3 件に届くまで）。
async fn pending_threads(
    db: &PgPool,
    tenant_id: &str,
    org: &str,
    limit: i64,
) -> Result<Vec<(Uuid, String, i32)>, sqlx::Error> {
    sqlx::query_as(
        "WITH c AS ( \
           SELECT t.id, t.owner, t.updated_at, \
                  (SELECT count(*) FROM message m \
                    WHERE m.thread_id = t.id AND m.role = 'user')::int AS n, \
                  ti.messages_seen \
           FROM thread t \
           LEFT JOIN thread_insight ti ON ti.tenant_id = t.tenant_id AND ti.thread_id = t.id \
           WHERE t.tenant_id = $1 AND t.org = $2 AND t.deleted_at IS NULL \
             AND t.updated_at > now() - interval '30 days' \
             AND (ti.thread_id IS NULL OR ti.messages_seen < $4) \
             AND EXISTS (SELECT 1 FROM generation_run g \
                          WHERE g.thread_id = t.id AND g.status = 'done') \
         ) \
         SELECT id, owner, n FROM c \
         WHERE n > 0 AND (messages_seen IS NULL OR n > messages_seen) \
         ORDER BY updated_at DESC LIMIT $3",
    )
    .bind(tenant_id)
    .bind(org)
    .bind(limit)
    .bind(i32::try_from(MESSAGES_FOR_CLASSIFY).unwrap_or(3))
    .fetch_all(db)
    .await
}

/// 会話の最初のユーザー発話の本文（text ブロックだけ・上限で切る）。
async fn excerpt(db: &PgPool, tenant_id: &str, thread_id: Uuid) -> Result<String, sqlx::Error> {
    let rows: Vec<(serde_json::Value,)> = sqlx::query_as(
        "SELECT content FROM message \
         WHERE thread_id = $1 AND tenant_id = $2 AND role = 'user' \
         ORDER BY created_at LIMIT $3",
    )
    .bind(thread_id)
    .bind(tenant_id)
    .bind(MESSAGES_FOR_CLASSIFY)
    .fetch_all(db)
    .await?;
    let mut out = String::new();
    for (content,) in rows {
        for block in content.as_array().into_iter().flatten() {
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    if !out.is_empty() {
                        out.push_str("\n---\n");
                    }
                    out.push_str(t);
                }
            }
        }
    }
    Ok(out.chars().take(EXCERPT_CHARS).collect())
}

/// LLM の出力からラベルを取り出す（見つからなければ `other`）。
pub fn parse_category(output: &str) -> &'static str {
    let lower = output.to_ascii_lowercase();
    // 長いキーから見る（部分一致の取り違えを避ける）。
    let mut keys: Vec<&'static str> = CATEGORIES.iter().map(|(k, _, _)| *k).collect();
    keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
    keys.into_iter()
        .find(|k| lower.contains(k))
        .unwrap_or("other")
}

/// 分類器の実行主体（組織に閉じる・利用者の集計からは除く）。
fn classifier_ctx(tenant_id: &str, org: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: CLASSIFIER_PRINCIPAL.to_string(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant_id.to_string()),
        },
        org.to_string(),
        tenant_id.to_string(),
    )
}

/// 1 件分類する（LLM 呼び出し → 会計 → 戻り値はラベル）。
async fn classify_one(
    gw: &LlmGateway,
    ctx: &AuthContext,
    model: &str,
    thread_id: Uuid,
    text: &str,
) -> Result<&'static str, String> {
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, text.to_string())]);
    req.model = Some(model.to_string());
    req.system = Some(SYSTEM_PROMPT.to_string());
    req.max_tokens = Some(16);
    let started = chrono::Utc::now();
    let mut stream = gw.stream(ctx, req).await.map_err(|e| e.to_string())?;
    let mut out = String::new();
    let mut usage = Usage::default();
    while let Some(item) = stream.next().await {
        match item.map_err(|e| e.to_string())? {
            StreamDelta::TextDelta { text } => out.push_str(&text),
            StreamDelta::Done { usage: u, .. } => usage = u,
            _ => {}
        }
    }
    let category = parse_category(&out);
    gw.record_generation(
        ctx,
        &GenerationRecord {
            idempotency_key: format!("insights:{thread_id}:{}", started.timestamp_millis()),
            model: model.to_string(),
            usage,
            trace_id: None,
            // 本文は Langfuse にも載せない（ラベルだけ）。
            input_preview: String::new(),
            output_preview: category.to_string(),
            app_id: None,
            source: UsageSource::Other,
            first_in_trace: true,
            started_at: Some(started),
        },
    )
    .await;
    Ok(category)
}

/// 1 組織分の未分類の会話を分類する（分類した件数を返す）。
pub async fn classify_org(
    db: &PgPool,
    gw: &LlmGateway,
    tenant_id: &str,
    org: &str,
    batch: i64,
    model_override: Option<&str>,
) -> Result<usize, sqlx::Error> {
    let threads = pending_threads(db, tenant_id, org, batch).await?;
    if threads.is_empty() {
        return Ok(0);
    }
    let ctx = classifier_ctx(tenant_id, org);
    // 組織が許可したモデル（設定の指定 → 組織の既定）。
    let model = match model_override {
        Some(m) => m.to_string(),
        None => match gw.resolve_choice(&ctx, &ModelChoice::default()).await? {
            Ok(r) => r.model,
            Err(e) => {
                tracing::warn!(error = %e, org, "用途分類: 使えるモデルがないため見送り");
                return Ok(0);
            }
        },
    };
    let mut done = 0usize;
    for (thread_id, owner, n) in threads {
        let text = excerpt(db, tenant_id, thread_id).await?;
        if text.trim().is_empty() {
            continue;
        }
        let category = match classify_one(gw, &ctx, &model, thread_id, &text).await {
            Ok(c) => c,
            Err(e) => {
                // 許可外・一時障害などはこの回は見送る（次回に再挑戦）。
                tracing::warn!(error = %e, org, "用途分類に失敗（この回は見送り）");
                break;
            }
        };
        sqlx::query(
            "INSERT INTO thread_insight \
             (tenant_id, org, thread_id, owner, category, messages_seen, model) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (tenant_id, thread_id) DO UPDATE SET \
               category = EXCLUDED.category, messages_seen = EXCLUDED.messages_seen, \
               model = EXCLUDED.model, classified_at = now()",
        )
        .bind(tenant_id)
        .bind(org)
        .bind(thread_id)
        .bind(&owner)
        .bind(category)
        .bind(n.min(i32::try_from(MESSAGES_FOR_CLASSIFY).unwrap_or(3)))
        .bind(&model)
        .execute(db)
        .await?;
        done += 1;
    }
    if done > 0 {
        // 会話本文を AI に渡したことを監査に残す（件数とモデルだけ）。
        let audit = AuditRecorder::new(db.clone());
        if let Err(e) = audit
            .record(
                &ctx,
                AuditEntry {
                    action: "insights.classify",
                    object_type: "organization",
                    object_id: &format!("{tenant_id}|{org}"),
                    decision: Decision::Allow,
                    trace_id: None,
                    metadata: json!({ "threads": done, "model": model }),
                },
            )
            .await
        {
            tracing::warn!(error = %e, "用途分類の監査記録に失敗");
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_picks_the_label_or_falls_back_to_other() {
        assert_eq!(parse_category("summarize"), "summarize");
        assert_eq!(parse_category(" Internal_Search\n"), "internal_search");
        assert_eq!(parse_category("ラベル: data_analysis"), "data_analysis");
        assert_eq!(parse_category("わかりません"), "other");
        // 部分一致の取り違え（research ⊂ ?）を起こさない。
        assert_eq!(parse_category("research"), "research");
    }
}
