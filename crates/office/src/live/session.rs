//! AI headless 参加による Office ライブ編集のサービス実体（issue #352）。
//!
//! `LiveEditor::apply` が 1 回のツール呼び出し＝1 headless セッションを担う:
//! ① editor@file の毎回再判定（HigherConsistency・不許可は存在秘匿へ畳む）
//! ② content_type ゲート（docx/xlsx/pptx のみ）
//! ③ **ファイル単位の AI 直列化**（Postgres advisory lock・AI↔AI のみ。人間↔AI は
//!    Collabora の共同編集に委ねて制限しない）＋同時セッション数の semaphore
//! ④ AI トークン（`token::issue_ai`）で CoolWSD へ接続 → ops 適用 → save → close
//!
//! ops 適用中の失敗は Err ではなく**部分適用の報告**（`LiveEditReport.aborted`）で
//! 返す（paste 非冪等・再送禁止・PIT-45）。保存は core ack 後に StorageService の
//! 版前進で検証する（shiki 自身が WOPI ホストなので観測できる）。

use std::sync::Arc;
use std::time::Duration;

use authz::{AuthContext, AuthzClient, Consistency, Relation};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgPool};
use storage::{NodeKind, StorageError, StorageService};
use uuid::Uuid;

use super::client::{CoolWsClient, CoolWsConfig, SaveAck};
use super::error::LiveError;
use super::ops::apply_op;
use crate::edit::EDITABLE_CONTENT_TYPES;
use crate::wopi::token::{self, OfficeTokenKey};

/// 同時 AI セッションの既定上限（kit プロセスはドキュメントごとに立つ＝メモリ保護）。
const DEFAULT_MAX_SESSIONS: usize = 4;

/// 直列化（advisory lock）・セッション枠の取得待ち上限。
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);

/// advisory lock のポーリング間隔。
const ACQUIRE_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// 保存後に版の前進を確認するポーリング上限（PutFile は save 直後に飛ぶ）。
const SAVE_VERIFY_TIMEOUT: Duration = Duration::from_secs(10);

/// ライブ編集の 1 操作（アンカー指定・ユーザーの選択に依存しない）。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum LiveOp {
    /// docx/pptx/xlsx: `find` を自 view で検索・選択照合し、HTML で置換する。
    ReplaceText { find: String, html: String },
    /// docx: 文書末尾へ HTML を追記する。
    AppendHtml { html: String },
    /// xlsx: `anchor`（例 "A1"・"Sheet2.B3"）起点に矩形の値を貼り込む。
    SetCells {
        anchor: String,
        rows: Vec<Vec<CellValue>>,
    },
}

impl LiveOp {
    /// 報告用のラベル。
    pub fn label(&self) -> &'static str {
        match self {
            LiveOp::ReplaceText { .. } => "replace_text",
            LiveOp::AppendHtml { .. } => "append_html",
            LiveOp::SetCells { .. } => "set_cells",
        }
    }
}

/// セルに貼り込む値（文字列 or 数値。数式は受けない＝データとして HTML エスケープ）。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum CellValue {
    Text(String),
    Number(f64),
    Bool(bool),
}

/// op 単位の適用結果（`office::EditOpResult` と同じ流儀）。
#[derive(Debug, Clone)]
pub struct LiveOpResult {
    pub op: &'static str,
    pub applied: bool,
    pub warning: Option<String>,
}

/// 保存の最終判定。
#[derive(Debug, Clone)]
pub enum LiveSaveResult {
    /// ストレージの版前進まで確認済み（新バージョン）。
    Saved { version: i64 },
    /// セッションへは適用済みだが、版前進を期限内に確認できなかった
    /// （他 view の保存/autosave で永続化される可能性が高い）。
    Unverified,
    /// 適用 0 件のため保存していない。
    NotAttempted,
    /// 保存が失敗した（編集はセッションに残り得る）。
    Failed(String),
}

/// 1 セッションの適用レポート（部分適用を正直に報告する）。
#[derive(Debug, Clone)]
pub struct LiveEditReport {
    pub file_name: String,
    pub results: Vec<LiveOpResult>,
    /// ops 途中でセッションが失敗した場合の理由（以降の op は未適用）。
    pub aborted: Option<String>,
    pub save: LiveSaveResult,
    /// 参加時点の総 view 数（1 なら AI 単独セッション）。
    pub views: u32,
}

/// ops 開始**前**の失敗（部分適用が無い＝安全にエラーへ写せる）。
#[derive(Debug, thiserror::Error)]
pub enum LiveEditError {
    /// 権限なし・未検出（存在秘匿へ畳む・#326）。
    #[error("対象にアクセスできません")]
    Denied,
    /// 対応外のファイル種別（authz 通過後のみ観測される）。
    #[error("この形式はライブ編集に対応していません（docx/xlsx/pptx のみ）")]
    Unsupported,
    /// 同一文書で別の AI 編集が進行中（直列化の待ち超過）。
    #[error("この文書では別の AI 編集が進行中です")]
    Busy,
    /// CoolWSD への接続・load 失敗（ops 前）。
    #[error("編集セッションを開始できません: {0}")]
    Session(#[from] LiveError),
    #[error("内部エラー: {0}")]
    Internal(String),
}

/// AI headless 参加のライブ編集サービス。
pub struct LiveEditor {
    storage: Arc<StorageService>,
    authz: Arc<dyn AuthzClient>,
    pool: PgPool,
    token_key: OfficeTokenKey,
    /// Collabora から見た shiki-server のベース URL（WOPISrc の組立て用）。
    wopi_base_url: String,
    ws: CoolWsConfig,
    sessions: tokio::sync::Semaphore,
}

impl LiveEditor {
    pub fn new(
        storage: Arc<StorageService>,
        authz: Arc<dyn AuthzClient>,
        pool: PgPool,
        token_key: OfficeTokenKey,
        wopi_base_url: impl Into<String>,
        ws: CoolWsConfig,
    ) -> Self {
        LiveEditor {
            storage,
            authz,
            pool,
            token_key,
            wopi_base_url: wopi_base_url.into(),
            ws,
            sessions: tokio::sync::Semaphore::new(DEFAULT_MAX_SESSIONS),
        }
    }

    /// アンカー指定の ops を開いている（または新規に開く）Collabora セッションへ適用する。
    pub async fn apply(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        ops: &[LiveOp],
    ) -> Result<LiveEditReport, LiveEditError> {
        // ① 実行主体の editor@file を毎回再判定（confused-deputy 回避・PIT-11）。
        let object = ctx.ns().file(&file_id.to_string());
        let allowed = self
            .authz
            .check(
                &ctx.subject(),
                Relation::Editor,
                &object,
                Consistency::HigherConsistency,
            )
            .await
            .map_err(|e| LiveEditError::Internal(format!("認可判定に失敗しました: {e}")))?;
        if !allowed {
            return Err(LiveEditError::Denied);
        }

        // ② content_type ゲート（editor 確認後なので種別の開示は存在秘匿を破らない）。
        let node = self
            .storage
            .get_metadata(ctx, file_id, None)
            .await
            .map_err(|e| match e {
                StorageError::Forbidden | StorageError::NotFound => LiveEditError::Denied,
                other => LiveEditError::Internal(other.to_string()),
            })?;
        let content_type = node.content_type.clone().unwrap_or_default();
        if node.kind != NodeKind::File || !EDITABLE_CONTENT_TYPES.contains(&content_type.as_str()) {
            return Err(LiveEditError::Unsupported);
        }

        // ③ 同時セッション枠 → ファイル単位の AI 直列化（AI↔AI のみ・レプリカ横断）。
        let _permit = tokio::time::timeout(ACQUIRE_TIMEOUT, self.sessions.acquire())
            .await
            .map_err(|_| LiveEditError::Busy)?
            .map_err(|e| LiveEditError::Internal(format!("セッション枠の取得に失敗: {e}")))?;
        let file_lock = FileAiLock::acquire(&self.pool, &ctx.tenant_id, file_id).await?;

        // ④ AI トークンで headless 接続（表示 identity は「Shiki AI」・routes 側）。
        let wopi_src = format!("{}/wopi/files/{file_id}", self.wopi_base_url);
        let access_token = token::issue_ai(&self.token_key, ctx, file_id)
            .map_err(|e| LiveEditError::Internal(format!("トークン発行に失敗: {e}")))?;
        let mut client = match CoolWsClient::connect(&self.ws, &wopi_src, &access_token, "ja").await
        {
            Ok(client) => client,
            Err(LiveError::Connect(first)) => {
                // load 前の接続失敗のみ 1 回だけ再接続する（冪等・PIT-45）。
                tracing::warn!(error = %first, "CoolWSD 接続失敗・再試行します");
                CoolWsClient::connect(&self.ws, &wopi_src, &access_token, "ja").await?
            }
            Err(other) => return Err(other.into()),
        };
        let views = client.loaded().views;

        // ⑤ ops を順に適用する。失敗は abort（以降未適用）として正直に報告する。
        let mut results: Vec<LiveOpResult> = Vec::with_capacity(ops.len());
        let mut aborted = None;
        for op in ops {
            match apply_op(&mut client, &content_type, op).await {
                Ok(result) => results.push(result),
                Err(e) => {
                    aborted = Some(format!("{}: {e}", op.label()));
                    break;
                }
            }
        }

        // ⑥ 1 件以上適用できたときのみ保存し、版の前進で永続化を検証する。
        let applied_any = results.iter().any(|r| r.applied);
        let save = if applied_any {
            self.save_and_verify(ctx, file_id, node.version, &mut client)
                .await
        } else {
            LiveSaveResult::NotAttempted
        };

        // ⑦ 必ず閉じる（kit プロセスのリーク防止）。advisory lock は guard の
        //    close で解放される。
        client.close().await;
        file_lock.release().await;

        Ok(LiveEditReport {
            file_name: node.name,
            results,
            aborted,
            save,
            views,
        })
    }

    /// save → core ack → StorageService の版前進を確認する。
    ///
    /// core の `savefailed` は誤検知があり得る（例: Calc の GoToCell が出す jsdialog
    /// 更新で background save が乱れて失敗報告→直後の PutFile 自体は成功する・実機で
    /// 確認）。save は冪等（paste と違い再送安全）なので、失敗時も**版の前進を正**として
    /// 検証し、進んでいなければ 1 回だけ save を再試行する。
    async fn save_and_verify(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        initial_version: i64,
        client: &mut CoolWsClient,
    ) -> LiveSaveResult {
        let mut last_error: Option<String> = None;
        for attempt in 0..2 {
            let ack = match client.save().await {
                Ok(ack) => Some(ack),
                Err(e) => {
                    tracing::warn!(error = %e, attempt, "CoolWSD save が失敗を報告（版前進で再検証）");
                    last_error = Some(e.to_string());
                    None
                }
            };
            // PutFile は shiki 自身に飛ぶため、版の前進が永続化の決定的な証拠になる。
            let deadline = tokio::time::Instant::now() + SAVE_VERIFY_TIMEOUT;
            loop {
                match self.storage.get_metadata(ctx, file_id, None).await {
                    Ok(node) if node.version > initial_version => {
                        return LiveSaveResult::Saved {
                            version: node.version,
                        };
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "保存検証の get_metadata に失敗");
                    }
                }
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            // ack 成功なのに版が進まない＝アップロード遅延。再試行せず未確認として返す。
            if matches!(ack, Some(SaveAck::CoreSaved | SaveAck::Unverified)) {
                return LiveSaveResult::Unverified;
            }
        }
        LiveSaveResult::Failed(
            last_error.unwrap_or_else(|| "保存に失敗しました（原因不明）".to_string()),
        )
    }
}

/// ファイル単位の AI 直列化（Postgres advisory lock・issue #352）。
///
/// 専用接続（pool から detach）で `pg_try_advisory_lock` をポーリング取得する。
/// 解放は接続クローズに一本化する（セッションレベルロックは接続終了で必ず解放
/// されるため、パニック・キャンセルでもロックが漏れない＝fail-safe）。
struct FileAiLock {
    conn: sqlx::PgConnection,
}

impl FileAiLock {
    async fn acquire(pool: &PgPool, tenant_id: &str, file_id: Uuid) -> Result<Self, LiveEditError> {
        let key = advisory_key(tenant_id, file_id);
        let mut conn = pool
            .acquire()
            .await
            .map_err(|e| LiveEditError::Internal(format!("DB 接続の取得に失敗: {e}")))?
            .detach();
        let deadline = tokio::time::Instant::now() + ACQUIRE_TIMEOUT;
        loop {
            let (locked,): (bool,) = sqlx::query_as("SELECT pg_try_advisory_lock($1)")
                .bind(key)
                .fetch_one(&mut conn)
                .await
                .map_err(|e| LiveEditError::Internal(format!("直列化ロックの取得に失敗: {e}")))?;
            if locked {
                return Ok(FileAiLock { conn });
            }
            if tokio::time::Instant::now() >= deadline {
                // 接続クローズで待ちを残さない。
                let _ = conn.close().await;
                return Err(LiveEditError::Busy);
            }
            tokio::time::sleep(ACQUIRE_POLL_INTERVAL).await;
        }
    }

    /// 接続を閉じてロックを解放する（Drop でも接続破棄で解放されるが、明示 close で
    /// サーバ側の後始末を確実にする）。
    async fn release(self) {
        let _ = self.conn.close().await;
    }
}

/// (tenant_id, file_id) → advisory lock キー（bigint）。SHA-256 先頭 8 バイト。
fn advisory_key(tenant_id: &str, file_id: Uuid) -> i64 {
    let mut hasher = Sha256::new();
    hasher.update(tenant_id.as_bytes());
    hasher.update(b":ai-live-edit:");
    hasher.update(file_id.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    i64::from_le_bytes(bytes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn live_op_deserializes_by_tag() {
        let ops: Vec<LiveOp> = serde_json::from_value(serde_json::json!([
            { "op": "replace_text", "find": "旧", "html": "<p>新</p>" },
            { "op": "append_html", "html": "<p>追記</p>" },
            { "op": "set_cells", "anchor": "B2", "rows": [["a", 1.5, true]] },
        ]))
        .unwrap();
        assert_eq!(ops.len(), 3);
        assert_eq!(ops[0].label(), "replace_text");
        // 未知フィールドは拒否（クローズド集合・スキーマ逸脱の早期検知）。
        assert!(serde_json::from_value::<LiveOp>(
            serde_json::json!({ "op": "replace_text", "find": "a", "html": "b", "evil": 1 })
        )
        .is_err());
    }



    #[test]
    fn advisory_key_is_stable_and_tenant_scoped() {
        let file = Uuid::new_v4();
        assert_eq!(advisory_key("t1", file), advisory_key("t1", file));
        assert_ne!(advisory_key("t1", file), advisory_key("t2", file));
        assert_ne!(advisory_key("t1", file), advisory_key("t1", Uuid::new_v4()));
    }
}
