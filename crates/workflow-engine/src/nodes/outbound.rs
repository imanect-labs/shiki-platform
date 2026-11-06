//! 外への呼び出し（`llm.invoke`・`http.request`・`Shiki.http.request`）の効果の記録（GAP-28・engine.md §7.3・§7.4）。
//!
//! リースが切れて step が拾い直されたとき（ワーカーが落ちた・リースを失った）に、前の実行が済ませた呼び出しを
//! 繰り返さない（LLM の二重課金・外部への二重送信）。手順は内部の書込（[`super::capability::journaled`]）と同じく
//! 予約 → 呼び出し → 結果の記録で、記録があれば呼ばずにその結果を返す。内部の書込と違うのは次の点。
//!
//! - 呼び出しが誤りを返したら、どの誤りでも予約を外す。やり直しは試す回数の決まりどおり呼び直す（作成者が
//!   やり直しを選んだ送信は、同じ `Idempotency-Key` でもう一度届く・§7.4）。
//! - 呼び出しを捨てたとき（step の時間の上限で打ち切った・panic）も予約を外す（[`ReleaseOnDrop`]）。予約が残るのは、
//!   ワーカーごと落ちたとき（とリースを失ったまま呼んでいるとき）だけ。
//! - 前の実行の予約が結果の無いまま残っていたら、試す回数を使わずに待つ（`effect_in_progress`・`retry::EFFECT_WAIT`）。
//!   回収の猶予を過ぎたら、LLM は呼び直し（費用だけで、外に何かが起きるわけではない）、外部への送信は届いたかが
//!   分からないので送り直さずに [`HTTP_DELIVERY_UNKNOWN`] で失敗にする（予約は消すので、再開とやり直しは送る）。
//! - 外部への送信は、送り先の状態を変えうる送信（GET・HEAD・OPTIONS 以外）だけを記録する（[`changes_remote`]）。
//!   読むだけの送信は送り直しても同じなので、記録の大きさを使わない。相手が誤り（400 以上）を返した応答も記録しない
//!   （受け付けられなかった送信なので、やり直しと再開は送り直す）。
//! - 記録するのはブロックの結果（LLM は `{ text, model[, data] }`、外部への送信は `{ status, host, body }`・本文は
//!   反射したシークレットの平文をマスクしたあとのもの）で、step の結果と同じもの。要求の本文・ヘッダー・シークレットは
//!   記録しない（操作の digest にハッシュで入るだけ）。結果が [`RECORD_MAX_BYTES`] を超えたら、LLM は記録しない
//!   （拾い直したら呼び直す）、外部への送信は本文を落とした印だけを記録する（拾い直したら届いたことは分かるが続きに
//!   渡せないので、送り直さずに [`HTTP_DELIVERY_UNKNOWN`] で失敗にする）。記録は step と同じく保持期間の GC で消える。
//! - 記録の DB が使えなければ呼ばない（`unavailable`・内部の書込と同じ fail-closed）。結果の記録に失敗したときは、
//!   呼び出しは済んでいるので結果を返す（拾い直しの窓だけ前と同じ at-least-once）。

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::capability::{op_digest, EffectJournal, JournalDecision, OutboundCheck};

use super::capability::{effect_conflict, effect_in_progress};
use super::http::HTTP_DELIVERY_UNKNOWN;
use super::ports::{LlmInvokeReq, PortError};

/// 外への呼び出しの結果として記録する大きさの上限（JSON のバイト数）。
pub const RECORD_MAX_BYTES: usize = 256 * 1024;

/// 本文を落として記録したことの印（外部への送信の結果の最上位の項目・相手の応答は `body` の下なので重ならない）。
const BODY_OMITTED: &str = "body_omitted";

/// 送り先の状態を変えうる送信か（記録して、拾い直しで送り直さない）。GET・HEAD・OPTIONS は読むだけの送信。
#[must_use]
pub fn changes_remote(method: &str) -> bool {
    !matches!(method, "GET" | "HEAD" | "OPTIONS")
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// `llm.invoke` の操作の digest（モデルの指定・指示・依頼・上限・思考の深さ・出力の形）。
///
/// モデルは指定のまま（省略は組織の既定）で、解決した名前は入れない（拾い直すまでに既定が変わっても同じ呼び出し）。
pub(crate) fn llm_digest(req: &LlmInvokeReq) -> String {
    op_digest(
        "llm.invoke",
        &json!({
            "model": req.model,
            "system": req.system,
            "prompt": req.prompt,
            "max_tokens": req.max_tokens,
            "effort": req.effort,
            "output_schema": req.output_schema,
        }),
    )
}

/// 外部への送信の操作の digest（方法・URL・本文・使うシークレットの名前）。
///
/// ヘッダーは入れない（署名や時刻を載せるヘッダーは送るたびに変わる）。本文はハッシュで入れる。
pub(crate) fn http_digest(
    method: &str,
    url: &str,
    body: Option<&[u8]>,
    secret: Option<&str>,
) -> String {
    op_digest(
        "http.request",
        &json!({
            "method": method,
            "url": url,
            "body_sha256": body.map(sha256_hex),
            "secret": secret,
        }),
    )
}

fn journal_unavailable(e: &crate::capability::JournalError) -> PortError {
    PortError::unavailable(format!("journal: {e}"))
}

/// 呼び出しを捨てたとき（打ち切り・panic で未来値が落ちた）に予約を外す。
///
/// 落とす側（`Drop`）では待てないので、外す処理は別のタスクで走らせる（実行時が無ければ何もしない）。ワーカーごと
/// 落ちたときは走らないので予約が残り、拾い直した実行が待つ。
struct ReleaseOnDrop {
    journal: EffectJournal,
    tenant_id: String,
    key: String,
    digest: String,
    armed: bool,
}

impl ReleaseOnDrop {
    fn new(journal: &EffectJournal, tenant_id: &str, key: &str, digest: &str) -> Self {
        ReleaseOnDrop {
            journal: journal.clone(),
            tenant_id: tenant_id.to_string(),
            key: key.to_string(),
            digest: digest.to_string(),
            armed: true,
        }
    }

    /// 呼び出しが戻った（ここからは呼び出し側が記録か予約を外すかを決める）。
    fn disarm(&mut self) {
        self.armed = false;
    }

    /// 予約を外す（呼び出しが誤りを返した・結果を記録しない）。外せなくても呼び出しの結果を優先する。
    async fn release_now(&self) {
        if let Err(e) = self
            .journal
            .release(&self.tenant_id, &self.key, &self.digest)
            .await
        {
            tracing::warn!(error = %e, key = %self.key,
                "外への呼び出しの予約を外せません（拾い直した実行は回収の猶予まで待つ）");
        }
    }

    /// 結果を記録する。記録できなくても呼び出しは済んでいるので結果を返す（拾い直しの窓だけ at-least-once）。
    async fn record(&self, result: &Value) {
        if let Err(e) = self
            .journal
            .record(&self.tenant_id, &self.key, &self.digest, result)
            .await
        {
            tracing::warn!(error = %e, key = %self.key,
                "外への呼び出しの結果を記録できません（拾い直すと呼び直すことがある）");
        }
    }
}

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let journal = self.journal.clone();
        let (tenant_id, key, digest) = (
            std::mem::take(&mut self.tenant_id),
            std::mem::take(&mut self.key),
            std::mem::take(&mut self.digest),
        );
        handle.spawn(async move {
            if let Err(e) = journal.release(&tenant_id, &key, &digest).await {
                tracing::warn!(error = %e, key = %key,
                    "捨てた外への呼び出しの予約を外せません（拾い直した実行は回収の猶予まで待つ）");
            }
        });
    }
}

fn fits(v: &Value) -> bool {
    serde_json::to_vec(v).is_ok_and(|b| b.len() <= RECORD_MAX_BYTES)
}

/// `llm.invoke` の呼び出し（修復を含む）を記録つきで 1 回にする。
///
/// 返すのは結果と、いま呼んだときの `call` の付随値（`None` は記録した結果を返した・監査はいま呼んだときだけ残す）。
/// 前の実行の予約が回収の猶予を過ぎていたら取り直して呼ぶ（[`EffectJournal::check`]）。
pub(crate) async fn llm_once<F, T>(
    journal: &EffectJournal,
    tenant_id: &str,
    key: &str,
    digest: &str,
    call: F,
) -> Result<(Value, Option<T>), PortError>
where
    F: std::future::Future<Output = Result<(Value, T), PortError>>,
{
    const WHAT: &str = "AI の呼び出し";
    match journal
        .check(tenant_id, key, digest)
        .await
        .map_err(|e| journal_unavailable(&e))?
    {
        JournalDecision::AlreadyDone(v) => Ok((v, None)),
        JournalDecision::InProgress => Err(effect_in_progress(WHAT)),
        JournalDecision::DigestMismatch => Err(effect_conflict(WHAT)),
        JournalDecision::Proceed => {
            let mut pending = ReleaseOnDrop::new(journal, tenant_id, key, digest);
            let result = call.await;
            pending.disarm();
            match result {
                Ok((out, extra)) => {
                    if fits(&out) {
                        pending.record(&out).await;
                    } else {
                        pending.release_now().await;
                    }
                    Ok((out, Some(extra)))
                }
                Err(e) => {
                    pending.release_now().await;
                    Err(e)
                }
            }
        }
    }
}

/// 外部への送信が届いたかを確かめられないときの誤り（[`HTTP_DELIVERY_UNKNOWN`]・retryable）。`sent` は、届いたことは
/// 分かっているが応答を記録していないとき。
fn delivery_unknown(host: &str, sent: bool) -> PortError {
    let head = if sent {
        format!(
            "外部のサービス（{host}）へ送りましたが、応答が大きいため記録しておらず、次のブロックに渡せません。"
        )
    } else {
        format!(
            "外部のサービス（{host}）へ送っている途中で処理が止まったため、届いたかを確かめられません。"
        )
    };
    PortError::new(
        HTTP_DELIVERY_UNKNOWN,
        format!(
            "{head}送り先の状態を確かめてから、必要なら失敗したところから再開してください。\
             再開すると、もう一度送ります。"
        ),
        true,
    )
}

/// 外部への送信（送り先の状態を変えうるもの・[`changes_remote`]）を記録つきで 1 回にする。
///
/// `send` は送って応答を確かめ、ブロックの結果 `{ status, host, body }` を返す（監査も中で残す）。`host` は誤りの文に
/// 出す送り先。返すのは結果（記録した結果を返したときも同じ形）。
pub(crate) async fn http_once<F>(
    journal: &EffectJournal,
    tenant_id: &str,
    key: &str,
    digest: &str,
    host: &str,
    send: F,
) -> Result<Value, PortError>
where
    F: std::future::Future<Output = Result<Value, PortError>>,
{
    const WHAT: &str = "外部のサービスへの送信";
    let decision = journal
        .check_outbound(tenant_id, key, digest)
        .await
        .map_err(|e| journal_unavailable(&e))?;
    let decision = match decision {
        OutboundCheck::Abandoned => return Err(delivery_unknown(host, false)),
        OutboundCheck::Decision(d) => d,
    };
    match decision {
        JournalDecision::AlreadyDone(v) if v.get(BODY_OMITTED) == Some(&Value::Bool(true)) => {
            // 届いたことは分かるが、続きに渡す本文が無い。印を消すので、次の実行（再開・やり直し）は送る。
            if let Err(e) = journal.discard(tenant_id, key, digest).await {
                tracing::warn!(error = %e, key, "本文を落とした記録を消せません");
            }
            Err(delivery_unknown(host, true))
        }
        JournalDecision::AlreadyDone(v) => Ok(v),
        JournalDecision::InProgress => Err(effect_in_progress(WHAT)),
        JournalDecision::DigestMismatch => Err(effect_conflict(WHAT)),
        JournalDecision::Proceed => {
            let mut pending = ReleaseOnDrop::new(journal, tenant_id, key, digest);
            let result = send.await;
            pending.disarm();
            match result {
                // 相手が誤りを返した応答（400 以上・`fail_on_error_status` を選ばないと成功として次へ渡る）は記録しない。
                // 受け付けられなかった送信なので、やり直し・再開・拾い直しは送り直す（script が誤りの応答を見て投げ、
                // やり直しで送り直す使い方を保つ・以前と同じ）。
                Ok(out) if error_status(&out) => {
                    pending.release_now().await;
                    Ok(out)
                }
                Ok(out) => {
                    if fits(&out) {
                        pending.record(&out).await;
                    } else {
                        pending.record(&omitted(&out)).await;
                    }
                    Ok(out)
                }
                Err(e) => {
                    pending.release_now().await;
                    Err(e)
                }
            }
        }
    }
}

/// 相手が誤りを返した応答か（状態コードが 400 以上）。
fn error_status(out: &Value) -> bool {
    out.get("status")
        .and_then(Value::as_u64)
        .is_some_and(|s| s >= 400)
}

/// 本文を落とした記録（状態コードと送り先だけ）。
fn omitted(out: &Value) -> Value {
    json!({
        "status": out.get("status"),
        "host": out.get("host"),
        BODY_OMITTED: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_reading_methods_are_left_unrecorded() {
        for m in ["GET", "HEAD", "OPTIONS"] {
            assert!(!changes_remote(m), "{m}");
        }
        for m in ["POST", "PUT", "PATCH", "DELETE", "PROPFIND"] {
            assert!(changes_remote(m), "{m}");
        }
    }

    /// digest は本文とシークレットの名前で変わり、ヘッダーには依らない（引数に無い）。
    #[test]
    fn http_digest_follows_the_body_and_the_secret() {
        let a = http_digest("POST", "https://x.test/a", Some(b"{\"n\":1}"), None);
        assert_eq!(
            a,
            http_digest("POST", "https://x.test/a", Some(b"{\"n\":1}"), None)
        );
        assert_ne!(
            a,
            http_digest("POST", "https://x.test/a", Some(b"{\"n\":2}"), None)
        );
        assert_ne!(
            a,
            http_digest("POST", "https://x.test/a", Some(b"{\"n\":1}"), Some("tok"))
        );
        assert_ne!(
            a,
            http_digest("PUT", "https://x.test/a", Some(b"{\"n\":1}"), None)
        );
        assert_ne!(a, http_digest("POST", "https://x.test/a", None, None));
    }

    #[test]
    fn error_responses_are_told_apart() {
        assert!(error_status(&json!({ "status": 400 })));
        assert!(error_status(&json!({ "status": 503 })));
        assert!(!error_status(&json!({ "status": 201 })));
        assert!(!error_status(&json!({ "status": 399 })));
    }

    #[test]
    fn large_results_are_recorded_without_the_body() {
        let big = json!({ "status": 201, "host": "x.test", "body": "あ".repeat(RECORD_MAX_BYTES) });
        assert!(!fits(&big));
        let mark = omitted(&big);
        assert_eq!(
            mark,
            json!({ "status": 201, "host": "x.test", "body_omitted": true })
        );
        assert!(fits(&mark));
        assert!(fits(
            &json!({ "status": 200, "host": "x.test", "body": "ok" })
        ));
    }

    #[test]
    fn delivery_unknown_says_resuming_sends_again() {
        for sent in [false, true] {
            let e = delivery_unknown("api.example.com", sent);
            assert_eq!(e.code, HTTP_DELIVERY_UNKNOWN);
            assert!(e.retryable);
            assert!(e.message.contains("api.example.com"), "{}", e.message);
            assert!(
                e.message.contains("再開すると、もう一度送ります。"),
                "{}",
                e.message
            );
        }
    }

    /// 記録しない journal（テスト用）では、毎回呼ぶ。
    #[tokio::test]
    async fn an_unrecorded_journal_always_calls() {
        let journal = EffectJournal::unrecorded();
        for _ in 0..2 {
            let (out, called) = llm_once(&journal, "t", "k", "d", async {
                Ok::<_, PortError>((json!({ "text": "hi" }), ()))
            })
            .await
            .unwrap();
            assert_eq!(out, json!({ "text": "hi" }));
            assert!(called.is_some());
        }
    }
}
