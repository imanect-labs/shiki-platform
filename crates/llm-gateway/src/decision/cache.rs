//! 判定の決定キャッシュ（ワークフローの ai.decide の step 属性 `cache`・FEAT-1 の T10・D-52 の H6）。
//!
//! 同じ決定器の版・同じ質問・同じ判定材料の答えを、同じワークフローの中で使い回す（既定は使わない）。
//!
//! - **キー**: `sha256(キーの版 ‖ 決定器の版 ‖ 正規化した質問 ‖ 正規化した判定材料)`。正規化はオブジェクトの
//!   キーを並べ替えた JSON（[`canonical`]）。決定器の版はカタログから構築時に決まるもの（llm_decider は
//!   `{元の実 ID}+llm_decider/{プロンプトの版}/k{回数}/{思考の深さ}`）で、プロンプト・回数・思考の深さ・元の
//!   モデルを変えれば別のキーになる。判定材料そのものは DB に持たない。
//! - **範囲**: テナント＋組織＋ワークフロー（[`DecisionCache::workflow_id`]）。組織全体で共有すると、他人が
//!   同じ文面を判定させたかを当たり外れで推測できる。
//! - **期限**: 書き込みから `ttl` まで。読むときはノードの今の `ttl` でも絞る（短くしたら古い答えは当たらない）。
//!   期限を過ぎた行は書き込みのたびにテナントの中で少しずつ消す。
//! - **掃除**: ワークフローを消すと、その行は同じトランザクションの中でトリガが消す（migration 0131）。期限切れ・
//!   実行履歴の保持期間より前に書いた行・ワークフローが無い行は、日次の実行履歴 GC（workflow-engine の
//!   run/store/gc.rs）がテナントごとに消す（書き込みが止まったテナントでも残さない）。
//! - 当たっても、組織の許可（判定用のモデルと元の chat モデルの両方）はチョークポイントが毎回確かめる。会計と
//!   利用上限は通さない（決定器を呼ばないので費用が出ない）。
//! - キャッシュの読み書きの失敗は判定を止めない（外れとして決定器を呼ぶ・書けなければ使い回さないだけ）。

use std::time::Duration;

use authz::AuthContext;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use super::{DecideRequest, DecideResponse};
use crate::model::Usage;

/// キーの組み方の版（組み方を変えたら上げる。古い行は当たらなくなり、期限で消える）。
const KEY_VERSION: &str = "shiki-decision-cache/v1";

/// 1 回の書き込みで消す期限切れの行の上限（書き込みを重くしない）。
const PURGE_BATCH: i64 = 100;

/// 決定キャッシュの範囲と期限（呼び出し側が渡す・ワークフローは step 属性 `cache` から組む）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecisionCache {
    /// 使い回す範囲のワークフロー（テナントと組織は `AuthContext` から取る）。
    pub workflow_id: uuid::Uuid,
    /// 答えを使い回す期間。
    pub ttl: Duration,
}

/// キャッシュを通した判定の結果（[`LlmGateway::decide_cached`]）。
///
/// [`LlmGateway::decide_cached`]: crate::gateway::LlmGateway::decide_cached
#[derive(Debug, Clone, PartialEq)]
pub struct Decided {
    pub response: DecideResponse,
    /// 決定キャッシュの答えを使い回したか（true なら決定器を呼んでおらず、使用量は 0）。
    pub cached: bool,
}

/// オブジェクトのキーを並べ替えた JSON（配列の順は保つ・数値と文字列はそのまま）。
#[must_use]
pub fn canonical(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|k| (k.clone(), canonical(&map[k])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

/// キャッシュのキー（`version` は決定器の版）。
#[must_use]
pub fn cache_key(version: &str, req: &DecideRequest) -> [u8; 32] {
    // 質問は中立形の JSON にしてから正規化する（並びは保つ・プロンプトの順になるため）。
    let questions = serde_json::to_value(&req.questions).unwrap_or(Value::Null);
    let material = Value::Array(vec![
        Value::from(KEY_VERSION),
        Value::from(version),
        canonical(&questions),
        canonical(&req.state),
    ]);
    // 配列の JSON なので、各部の境目が曖昧にならない（文字列の連結で別の組と同じ並びにならない）。
    Sha256::digest(material.to_string().as_bytes()).into()
}

/// 期間を秒（DB の interval）へ。
fn secs(ttl: Duration) -> f64 {
    ttl.as_secs_f64()
}

/// 決定キャッシュの読み書き（`llm_decision_cache`）。
#[derive(Clone)]
pub(crate) struct DecisionCacheStore {
    db: PgPool,
}

impl DecisionCacheStore {
    pub(crate) fn new(db: PgPool) -> Self {
        DecisionCacheStore { db }
    }

    /// 期限内の答えを引く（無ければ `None`）。使用量は 0 で返す。
    pub(crate) async fn get(
        &self,
        ctx: &AuthContext,
        scope: &DecisionCache,
        key: &[u8; 32],
    ) -> Result<Option<DecideResponse>, sqlx::Error> {
        let row: Option<(sqlx::types::Json<DecideResponse>,)> = sqlx::query_as(
            "SELECT response FROM llm_decision_cache \
              WHERE tenant_id = $1 AND org = $2 AND workflow_id = $3 AND cache_key = $4 \
                AND expires_at > now() AND created_at > now() - make_interval(secs => $5)",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(scope.workflow_id)
        .bind(key.as_slice())
        .bind(secs(scope.ttl))
        .fetch_optional(&self.db)
        .await?;
        Ok(row.map(|(r,)| {
            let mut resp = r.0;
            resp.usage = Usage::default();
            resp
        }))
    }

    /// 答えを書く（同じキーは置き換える）。あわせて、テナントの期限切れの行を少し消す。
    pub(crate) async fn put(
        &self,
        ctx: &AuthContext,
        scope: &DecisionCache,
        key: &[u8; 32],
        resp: &DecideResponse,
    ) -> Result<(), sqlx::Error> {
        // 使用量は持たない（当たりは費用が出ない・会計の値を二重に持たない）。
        let stored = DecideResponse {
            usage: Usage::default(),
            ..resp.clone()
        };
        sqlx::query(
            "INSERT INTO llm_decision_cache \
               (tenant_id, org, workflow_id, cache_key, resolved_model, response, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, now() + make_interval(secs => $7)) \
             ON CONFLICT (tenant_id, org, workflow_id, cache_key) DO UPDATE \
               SET resolved_model = excluded.resolved_model, response = excluded.response, \
                   created_at = now(), expires_at = excluded.expires_at",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(scope.workflow_id)
        .bind(key.as_slice())
        .bind(&stored.resolved_model)
        .bind(sqlx::types::Json(&stored))
        .bind(secs(scope.ttl))
        .execute(&self.db)
        .await?;
        sqlx::query(
            "DELETE FROM llm_decision_cache WHERE ctid = ANY(ARRAY( \
               SELECT ctid FROM llm_decision_cache \
                WHERE tenant_id = $1 AND expires_at <= now() \
                ORDER BY expires_at LIMIT $2))",
        )
        .bind(ctx.tenant_id())
        .bind(PURGE_BATCH)
        .execute(&self.db)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::{ChoiceOption, Question, QuestionKind};

    fn request(state: Value) -> DecideRequest {
        DecideRequest {
            model: "decide".into(),
            state,
            questions: vec![Question {
                key: "category".into(),
                instructions: "種類".into(),
                kind: QuestionKind::Choice {
                    options: vec![
                        ChoiceOption {
                            key: "billing".into(),
                            description: "請求".into(),
                        },
                        ChoiceOption {
                            key: "bug".into(),
                            description: "不具合".into(),
                        },
                    ],
                },
            }],
        }
    }

    #[test]
    fn canonical_sorts_object_keys_and_keeps_array_order() {
        let v = serde_json::json!({ "b": 1, "a": { "y": [3, 1], "x": null } });
        assert_eq!(
            canonical(&v).to_string(),
            r#"{"a":{"x":null,"y":[3,1]},"b":1}"#
        );
    }

    /// 判定材料のキーの並びはキーを変えない。版・材料・質問・質問の並びが変われば別のキー。
    #[test]
    fn the_key_follows_the_version_the_questions_and_the_state() {
        let base = cache_key("v1", &request(serde_json::json!({ "a": 1, "b": "請求" })));
        assert_eq!(
            base,
            cache_key("v1", &request(serde_json::json!({ "b": "請求", "a": 1 })))
        );
        assert_ne!(
            base,
            cache_key("v2", &request(serde_json::json!({ "a": 1, "b": "請求" })))
        );
        assert_ne!(
            base,
            cache_key("v1", &request(serde_json::json!({ "a": 2, "b": "請求" })))
        );
        let mut other = request(serde_json::json!({ "a": 1, "b": "請求" }));
        other.questions[0].instructions = "種類は".into();
        assert_ne!(base, cache_key("v1", &other));
        let mut reordered = request(serde_json::json!({ "a": 1, "b": "請求" }));
        if let QuestionKind::Choice { options } = &mut reordered.questions[0].kind {
            options.reverse();
        }
        assert_ne!(
            base,
            cache_key("v1", &reordered),
            "選択肢の並びはプロンプトに効く"
        );
        // 文字列の境目をずらしても同じ並びにならない（版と材料の連結で別の組と衝突しない）。
        assert_ne!(
            cache_key("v1", &request(serde_json::json!("x"))),
            cache_key("v1x", &request(serde_json::json!("")))
        );
    }
}
