//! StorageService: 社内文書の検索への取り込み直しの認可（D-126・GAP-51）。
//!
//! 取り込み直しは内容を変えないが、ファイルの持ち主の側の操作として扱う（取り込みの負荷をかけ、監査に残る）。
//! 頼めるのは**ファイルを編集できる人**（`editor@file`・内容の更新と同じ relation）。見えない・無い・ごみ箱の
//! ファイルとフォルダーは同じ `NotFound`（存在を秘匿する）、見えるが編集できない人は `Forbidden`。
//!
//! ここは認可と対象の確定だけを行い、取り込みの待ち行列へ積むのは社内文書の検索（`rag` の
//! `SearchService::request_reindex`）が、取り込みの状態を確かめた同じ Tx で outbox に [`WriteOp::Reindex`] を
//! 発行して行う（取り込みの状態は rag が持つ・書込イベントの経路は既存のまま）。
//!
//! [`WriteOp::Reindex`]: crate::event::WriteOp::Reindex
//!
//! `service.rs`（親）が持つ struct/フィールド/自由関数を `use super::*` で参照する。

// 分割した impl ブロック。親 `service.rs` の struct/フィールド/自由関数/型 import を総取りする。
#[allow(clippy::wildcard_imports)]
use super::*;
use crate::audit::AuditAction;

impl StorageService {
    /// `file_id` を社内文書の検索に取り込み直してよいかを確かめ、対象のノードを返す（`editor@file`）。
    ///
    /// - 無い・ごみ箱・フォルダー・見えないファイル: [`StorageError::NotFound`]（見えないの拒否は監査に残す）。
    /// - 見えるが編集できない: [`StorageError::Forbidden`]（拒否は監査に残す）。
    ///
    /// 許可の監査は、取り込みへ積んだときに積んだ側が同じ Tx で残す（断ったときに許可の行を残さない）。
    pub async fn authorize_reindex(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        let node = self.load_node(ctx, file_id, false).await?;
        // フォルダーは取り込みの単位ではない（中のファイルを 1 件ずつ頼む）。見えない・無いと同じに言う。
        if node.kind != NodeKind::File {
            return Err(StorageError::NotFound);
        }
        let object = ctx.ns().file(&file_id.to_string());
        let id = file_id.to_string();
        // 共有リンクの遅延失効（#342）: 期限切れのタプルで通らないよう、判定の前に剥奪する（メタ取得と同じ）。
        self.enforce_share_link_expiry(ctx, file_id, node.kind)
            .await?;
        self.require_read(
            ctx,
            &object,
            AuditAction::FileReindex,
            "file",
            &id,
            trace_id,
        )
        .await?;
        self.require(
            ctx,
            Relation::Editor,
            &object,
            AuditAction::FileReindex,
            "file",
            &id,
            trace_id,
        )
        .await?;
        Ok(node)
    }
}
