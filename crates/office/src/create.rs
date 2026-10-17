//! Office ファイルの新規作成（#381）。
//!
//! 「新規作成」は **空テンプレを実体化 → Collabora へ本文を paste** の 2 段で行う。
//! md→docx の自前サブセット変換（`append_markdown`）は新規作成経路から外した:
//! 太字/リンク/ネスト箇条書き/引用が落ちる劣化コピーを保守するより、既にある本物の
//! エンジン（LibreOffice）に HTML を渡して docx 化させる方が忠実度が高い（#381 の決定）。
//!
//! 認可は 2 段とも既存のチョークポイントを通る:
//! - 作成: [`StorageService::write_file_unique_internal`]（親フォルダ ReBAC＋監査＋版）
//! - 本文: [`LiveEditor::apply`]（毎回 `editor@file` を OpenFGA 再判定・PIT-11）
//!
//! いずれも**発話ユーザーの `AuthContext`** で実行し昇格しない（confused-deputy 回避）。

use std::sync::Arc;

use authz::AuthContext;
use storage::StorageService;
use uuid::Uuid;

use crate::error::OfficeError;
use crate::live::{LiveEditError, LiveEditReport, LiveEditor, LiveOp};
use crate::templates::OfficeKind;

/// 作成した Office ファイルの参照（チャットの document_ref カード・遷移先の素）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedOffice {
    pub node_id: Uuid,
    /// 実際に付いたファイル名（同名衝突で ` (2)` が付くことがある）。
    pub name: String,
    pub kind: OfficeKind,
    /// 作成直後のバージョン（本文流し込みで前進した場合は呼び出し側が更新する）。
    pub version: i64,
}

/// 新規 Office ファイルの作成＋初期本文流し込み。
pub struct OfficeCreator {
    storage: Arc<StorageService>,
    live: Arc<LiveEditor>,
}

impl OfficeCreator {
    pub fn new(storage: Arc<StorageService>, live: Arc<LiveEditor>) -> Self {
        OfficeCreator { storage, live }
    }

    /// 空テンプレを実体化し、`ops` があれば Collabora セッションで本文を流し込む。
    ///
    /// 本文の流し込みが失敗しても**作成済みファイルは巻き戻さない**（Collabora で開けば
    /// ユーザーが続きを書ける空文書として残る方が、黙って消えるより回復可能）。失敗は
    /// `Err` ではなく `(created, Err(..))` として返し、呼び出し側が正直に報告する。
    pub async fn create(
        &self,
        ctx: &AuthContext,
        parent_id: Option<Uuid>,
        name: &str,
        kind: OfficeKind,
        ops: &[LiveOp],
        trace_id: Option<&str>,
    ) -> Result<(CreatedOffice, Option<Result<LiveEditReport, LiveEditError>>), OfficeError> {
        let file_name = kind.file_name(name);
        let node = self
            .storage
            .write_file_unique_internal(
                ctx,
                parent_id,
                &file_name,
                kind.blank(),
                kind.content_type(),
                trace_id,
            )
            .await?;
        let created = CreatedOffice {
            node_id: node.id,
            name: node.name,
            kind,
            version: node.version,
        };
        if ops.is_empty() {
            return Ok((created, None));
        }
        let report = self.live.apply(ctx, created.node_id, ops).await;
        Ok((created, Some(report)))
    }
}
