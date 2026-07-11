//! プロセス内ドキュメントハブ（ロード・共有・アンロードと authz ゲート）。
//!
//! authz はドキュメント単位＝対応する node の ReBAC（`file:<id>` の viewer/editor）。
//! 接続時に必ず [`CollabHub::authorize`] を通し、セッション中も定期再チェックする
//! （PIT-37②: 接続時 1 回だと共有解除後も update が流れ続ける）。剥奪の即時反映が
//! 要る経路のため整合性は常に `HigherConsistency`（PIT-11 と同じ扱い）。

use std::collections::HashMap;
use std::sync::Arc;

use authz::{AuthContext, AuthzClient, Consistency, Relation};
use storage::{Node, NodeKind, StorageService};
use uuid::Uuid;

use crate::doc::LiveDoc;
use crate::error::CollabError;
use crate::note;
use crate::store::DocStore;

/// ロード済みドキュメント 1 件（ノートの場合は自動保存タスクを併走させる）。
struct DocEntry {
    doc: Arc<LiveDoc>,
    saver: Option<tokio::task::JoinHandle<()>>,
}

/// 接続主体に許すアクセスモード。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    /// 読み書き（update の適用を許可）。
    Editor,
    /// 読み取りのみ（update は受理しない・awareness は許可）。
    Viewer,
}

/// セッション中の権限再チェック間隔の既定値（PIT-37②・WOPI と同じ「定期再チェック」）。
const DEFAULT_RECHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// ライブドキュメントの共有ハブ。1 プロセスに 1 つ（AppState 常駐）。
pub struct CollabHub {
    docs: tokio::sync::Mutex<HashMap<Uuid, DocEntry>>,
    store: DocStore,
    authz: Arc<dyn AuthzClient>,
    storage: Arc<StorageService>,
    recheck_interval: std::time::Duration,
}

impl CollabHub {
    pub fn new(
        pool: sqlx::PgPool,
        authz: Arc<dyn AuthzClient>,
        storage: Arc<StorageService>,
    ) -> Self {
        CollabHub {
            docs: tokio::sync::Mutex::new(HashMap::new()),
            store: DocStore::new(pool),
            authz,
            storage,
            recheck_interval: DEFAULT_RECHECK_INTERVAL,
        }
    }

    /// 権限再チェック間隔を差し替える（テストで剥奪切断を短時間に検証するため）。
    #[must_use]
    pub fn with_recheck_interval(mut self, interval: std::time::Duration) -> Self {
        self.recheck_interval = interval;
        self
    }

    pub fn recheck_interval(&self) -> std::time::Duration {
        self.recheck_interval
    }

    pub fn store(&self) -> &DocStore {
        &self.store
    }

    /// 接続時・定期再チェック共通の認可判定（fail-closed）。
    ///
    /// editor → 読み書き、viewer → 読み取りのみ、どちらも無ければ Forbidden。
    pub async fn authorize(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
    ) -> Result<AccessMode, CollabError> {
        let object = ctx.ns().file(&node_id.to_string());
        let subject = ctx.subject();
        if self
            .authz
            .check(
                &subject,
                Relation::Editor,
                &object,
                Consistency::HigherConsistency,
            )
            .await?
        {
            return Ok(AccessMode::Editor);
        }
        if self
            .authz
            .check(
                &subject,
                Relation::Viewer,
                &object,
                Consistency::HigherConsistency,
            )
            .await?
        {
            return Ok(AccessMode::Viewer);
        }
        Err(CollabError::Forbidden(format!("file {node_id}")))
    }

    /// ノード実在＋ファイル種別の確認（viewer チェック・監査込み＝StorageService 経由）。
    pub async fn require_file(
        &self,
        ctx: &AuthContext,
        node_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<storage::Node, CollabError> {
        let node = self.storage.get_metadata(ctx, node_id, trace_id).await?;
        if node.kind != NodeKind::File {
            return Err(CollabError::NotFound(format!("file {node_id}")));
        }
        Ok(node)
    }

    /// ドキュメントに参加する（未ロードなら永続状態から復元）。
    ///
    /// 認可済みであること（[`Self::authorize`]）を呼び出し側の責務とする。
    /// ノート（.md）は初回ロード時に外部書込のインポート判定を行い（Task 11P.2 の
    /// 単方向規約）、デバウンス保存タスクを併走させる。
    pub async fn join(&self, ctx: &AuthContext, node: &Node) -> Result<Arc<LiveDoc>, CollabError> {
        let node_id = node.id;
        let mut docs = self.docs.lock().await;
        let doc = if let Some(entry) = docs.get(&node_id) {
            Arc::clone(&entry.doc)
        } else {
            let persisted = self
                .store
                .load_or_init(node_id, &node.org, &node.tenant_id)
                .await?;
            let live = Arc::new(LiveDoc::restore(node_id, &persisted)?);
            let saver = if note::is_note_file(&node.name) {
                note::saver::import_if_stale(
                    &live,
                    &self.store,
                    &self.storage,
                    ctx,
                    node_id,
                    node.version,
                    persisted.saved_node_version,
                )
                .await?;
                Some(note::saver::spawn(
                    Arc::clone(&live),
                    self.store.clone(),
                    Arc::clone(&self.storage),
                ))
            } else {
                None
            };
            docs.insert(
                node_id,
                DocEntry {
                    doc: Arc::clone(&live),
                    saver,
                },
            );
            live
        };
        doc.conn_joined();
        Ok(doc)
    }

    /// ドキュメントから離脱する。最終接続なら保存を flush し、最終圧縮してアンロードする。
    pub async fn leave(&self, doc: &Arc<LiveDoc>) {
        if doc.conn_left() > 0 {
            return;
        }
        let mut docs = self.docs.lock().await;
        // ロック取得までの間に新規参加があり得るため、ロック下で再確認する。
        if doc.conn_count() > 0 {
            return;
        }
        let entry = docs.remove(&doc.node_id);
        drop(docs);
        if let Some(DocEntry {
            saver: Some(saver), ..
        }) = entry
        {
            saver.abort();
        }
        // 未保存編集があれば md へ最終保存する（デバウンス待ちを打ち切る）。
        if let Err(e) = note::saver::save_note(doc, &self.store, &self.storage).await {
            tracing::warn!(node_id = %doc.node_id, error = %e,
                "アンロード時のノート保存に失敗（Yjs 側は保全済み）");
        }
        if let Err(e) = doc.compact_now(&self.store).await {
            tracing::warn!(node_id = %doc.node_id, error = %e, "最終圧縮に失敗（log は保全済み）");
        }
    }

    /// ノートを即時保存する（テスト・明示保存用）。返り値は保存時の新 version。
    pub async fn save_note_now(&self, doc: &Arc<LiveDoc>) -> Result<Option<i64>, CollabError> {
        note::saver::save_note(doc, &self.store, &self.storage).await
    }

    /// ノート本文を正規化 md で読む（`document.read`・Task 11P.4）。viewer 以上で可。
    pub async fn read_note_markdown(
        &self,
        ctx: &AuthContext,
        node: &Node,
    ) -> Result<String, CollabError> {
        if node.kind != NodeKind::File || !note::is_note_file(&node.name) {
            return Err(CollabError::NotFound(format!("note {}", node.id)));
        }
        // viewer 認可（読めなければ Forbidden→存在秘匿は呼び出し側で 404 化）。
        self.authorize(ctx, node.id).await?;
        let doc = self.join(ctx, node).await?;
        let markdown = doc.to_markdown();
        self.leave(&doc).await;
        markdown
    }

    /// AI（`document.edit`）の編集を共有ドキュメントへ適用する（Task 11P.4）。
    ///
    /// 権限は**実行主体の editor@file**（人間と同一経路・viewer は拒否）。編集は共有
    /// LiveDoc に適用され（Yjs が人間の並行編集と収束させる）、生成 update を永続化＋
    /// 接続中の人間へブロードキャストする。ファイル直接上書きの経路は作らない。
    pub async fn apply_ai_edit(
        &self,
        ctx: &AuthContext,
        node: &Node,
        ops: &[note::EditOp],
        mode: note::EditMode,
    ) -> Result<note::EditReport, CollabError> {
        // ファイル種別＋実在（viewer 認可・監査つき）。
        if node.kind != NodeKind::File || !note::is_note_file(&node.name) {
            return Err(CollabError::NotFound(format!("note {}", node.id)));
        }
        // editor 権限（human と同一経路・fail-closed）。
        match self.authorize(ctx, node.id).await? {
            AccessMode::Editor => {}
            AccessMode::Viewer => {
                return Err(CollabError::Forbidden(format!(
                    "editor 権限がありません: {}",
                    node.id
                )))
            }
        }
        let doc = self.join(ctx, node).await?;
        let result = self.apply_ai_edit_locked(&doc, ctx, ops, mode).await;
        // AI 編集は一過性の参加。leave が最終接続なら保存＋アンロードまで面倒を見る。
        self.leave(&doc).await;
        result
    }

    /// join 済みドキュメントへ AI 編集を適用し、永続化＋ブロードキャストする。
    async fn apply_ai_edit_locked(
        &self,
        doc: &Arc<LiveDoc>,
        ctx: &AuthContext,
        ops: &[note::EditOp],
        mode: note::EditMode,
    ) -> Result<note::EditReport, CollabError> {
        let (update, report) = doc.apply_ai_edit(ops, mode)?;
        doc.persist_ai_update(&self.store, &update, ctx).await?;
        // 接続中の人間へライブ配信する（from=0 は合成 id・全接続が受信）。
        doc.broadcast(0, ai_sync_update_frame(&update));
        Ok(report)
    }
}

/// update バイト列を y-sync の `Update` メッセージフレームに包む（クライアントが解釈できる形）。
fn ai_sync_update_frame(update: &[u8]) -> Vec<u8> {
    use yrs::sync::{Message, SyncMessage};
    use yrs::updates::encoder::Encode;
    Message::Sync(SyncMessage::Update(update.to_vec())).encode_v1()
}
