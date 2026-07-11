//! ノートの保存（デバウンス）とインポート（Task 11P.2）。
//!
//! - **保存**: 編集がアイドル [`SAVE_IDLE`] 続く、または最初の未保存編集から
//!   [`SAVE_MAX`] 経過したら、md へシリアライズして StorageService の新バージョンに
//!   書く（→ 書込イベント → RAG 再索引が既存経路で動く）。保存の実行主体は
//!   **最後に編集した人間/AI の AuthContext**（editor relation は書込側で再判定される）。
//! - **インポート**: ロード時に `node.version` が前回保存版（saved_node_version）から
//!   進んでいれば、ファイル側の外部書込があったとみなし md を Yjs へ全置換で取り込む。
//!   セッション中の外部書込は取り込まない（Yjs が真実・外部版は履歴に残る）。

use std::sync::Arc;
use std::time::Duration;

use storage::StorageService;
use uuid::Uuid;

use crate::doc::LiveDoc;
use crate::error::CollabError;
use crate::store::DocStore;

/// 編集アイドルでの保存デバウンス。
pub const SAVE_IDLE: Duration = Duration::from_secs(3);
/// 未保存編集の最大滞留（連続編集中でもこの間隔で保存する）。
pub const SAVE_MAX: Duration = Duration::from_secs(30);
/// 保存判定のポーリング間隔。
const TICK: Duration = Duration::from_secs(1);

/// デバウンス保存ループを起動する（ドキュメントのアンロード時に abort される）。
pub(crate) fn spawn(
    doc: Arc<LiveDoc>,
    store: DocStore,
    storage: Arc<StorageService>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if doc.note_should_save(SAVE_IDLE, SAVE_MAX) {
                if let Err(e) = save_note(&doc, &store, &storage).await {
                    tracing::warn!(node_id = %doc.node_id, error = %e,
                        "ノートの自動保存に失敗（次の tick で再試行）");
                }
            }
        }
    })
}

/// ノートを md へシリアライズして新バージョン保存する（dirty でなければ no-op）。
///
/// 返り値は保存した場合の新しい node.version。
pub async fn save_note(
    doc: &Arc<LiveDoc>,
    store: &DocStore,
    storage: &Arc<StorageService>,
) -> Result<Option<i64>, CollabError> {
    let Some(ctx) = doc.note_take_dirty() else {
        return Ok(None);
    };
    let markdown = doc.to_markdown()?;
    match storage
        .update_file_content_internal(
            &ctx,
            doc.node_id,
            markdown.as_bytes(),
            "text/markdown",
            None,
        )
        .await
    {
        Ok(node) => {
            store
                .set_saved_node_version(doc.node_id, node.version)
                .await?;
            Ok(Some(node.version))
        }
        Err(e) => {
            // 失敗時は dirty に戻し、次の tick / アンロード時に再試行する。
            doc.note_mark_dirty(&ctx);
            Err(CollabError::Storage(e))
        }
    }
}

/// ロード時のインポート判定と実行（外部書込の単方向取り込み）。
///
/// `saved_node_version` が現在の `node_version` と異なる場合のみ、ファイル内容を
/// Yjs へ全置換で取り込み、取り込み後の全状態を snapshot として即時永続化する。
#[allow(clippy::too_many_arguments)] // ロード文脈の値を束ねず素で受ける（呼び出し元は 1 箇所＋テスト）。
pub async fn import_if_stale(
    doc: &Arc<LiveDoc>,
    store: &DocStore,
    storage: &Arc<StorageService>,
    ctx: &authz::AuthContext,
    node_id: Uuid,
    node_version: i64,
    saved_node_version: Option<i64>,
    // ロード時点の最終 seq（= 既存 update 列の消し込み上限。呼び出し元が確定値で渡す）。
    upto_seq: i64,
) -> Result<(), CollabError> {
    if saved_node_version == Some(node_version) {
        return Ok(());
    }
    let (_node, bytes) = storage.read_file_internal(ctx, node_id, None).await?;
    let markdown = String::from_utf8_lossy(&bytes);
    doc.import_markdown(&markdown)?;
    // インポート結果は update log を経ないため、snapshot として即時に正本へ落とす。
    // ロード時（共有前・並行 append 無し）なので upto_seq は確定値でよい。
    let snapshot = doc.full_state()?;
    store
        .overwrite_snapshot(node_id, &snapshot, upto_seq)
        .await?;
    store.set_saved_node_version(node_id, node_version).await?;
    Ok(())
}
