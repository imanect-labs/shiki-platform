//! セッション（認可・直列化・保存）に包んだエンジンの入口（#725）。
//!
//! 1 回のツール呼び出し＝1 headless セッション（`LiveEditor::with_session`）。権限は毎回
//! OpenFGA で再判定され（読み取りは viewer・編集は editor）、編集は同じファイルの AI 編集と
//! 直列化される。編集が文書を変えたときだけ保存し、版の前進で永続化を確かめる。

use std::sync::Arc;

use authz::AuthContext;
use uuid::Uuid;

use super::a1;
use super::calc::normalize::{normalize, OpError};
use super::calc::ops::SheetOp;
use super::calc::run;
use super::EngineError;
use crate::live::{LiveEditError, LiveEditor, LiveSaveResult, SessionAccess};
use crate::templates::XLSX_CONTENT_TYPE;

/// エンジンで扱う表計算の形式。
pub const SHEET_CONTENT_TYPES: &[&str] = &[
    XLSX_CONTENT_TYPE,
    "application/vnd.oasis.opendocument.spreadsheet",
];

/// 入口の失敗（LLM へ返す文言に写す）。
#[derive(Debug, thiserror::Error)]
pub enum OfficeEngineError {
    /// 操作の内容が不正（セッションを開く前に検出・文書は変わっていない）。
    #[error(transparent)]
    Invalid(#[from] OpError),
    /// 範囲の書き方が不正（読み取り）。
    #[error(transparent)]
    Range(#[from] a1::A1Error),
    /// 権限・形式・混雑・接続の失敗。
    #[error(transparent)]
    Session(#[from] LiveEditError),
    /// エンジンの失敗（セッション切断・kit の例外）。
    #[error(transparent)]
    Engine(#[from] EngineError),
}

/// Excel 編集の報告。
#[derive(Debug, Clone)]
pub struct SheetEditReport {
    pub file_name: String,
    /// LLM への観測テキスト（適用結果・読み戻し・検査）。
    pub text: String,
    /// 文書を変えたか。
    pub applied: bool,
    pub save: LiveSaveResult,
    /// 参加時点の総 view 数（1 なら AI 単独セッション）。
    pub views: u32,
}

/// 読み取りの指定。
#[derive(Debug, Clone, Default)]
pub struct SheetReadRequest {
    /// 読む範囲（省略でブックの概要＋作業中シートの先頭）。
    pub range: Option<String>,
    /// 範囲にシート名が無いときのシート。
    pub sheet: Option<String>,
    pub max_cells: Option<u32>,
    /// 条件付き書式・グラフ・印刷設定・固定などシート全体の付帯情報も読む。
    pub details: bool,
}

/// エンジンの入口。
pub struct OfficeEngine {
    live: Arc<LiveEditor>,
}

impl OfficeEngine {
    pub fn new(live: Arc<LiveEditor>) -> Self {
        OfficeEngine { live }
    }

    /// Excel を読む（viewer@file）。
    pub async fn read_sheet(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        req: &SheetReadRequest,
    ) -> Result<String, OfficeEngineError> {
        let area = req.range.as_deref().map(a1::parse_area).transpose()?;
        let max_cells = req
            .max_cells
            .unwrap_or(run::DEFAULT_MAX_CELLS)
            .clamp(1, 5_000);
        let outcome = Box::pin(self.live.with_session(
            ctx,
            file_id,
            SessionAccess::Read,
            SHEET_CONTENT_TYPES,
            async |client, info| {
                let r = match &area {
                    Some(a) => {
                        run::read_range(client, a, req.sheet.as_deref(), max_cells, req.details)
                            .await
                    }
                    None => run::read_overview(client, &info.file_name).await,
                };
                (r, false)
            },
        ))
        .await?;
        Ok(outcome.value?)
    }

    /// Excel を編集する（editor@file）。操作は全部成功か全部取り消し。
    pub async fn edit_sheet(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        label: &str,
        default_sheet: Option<&str>,
        ops: &[SheetOp],
    ) -> Result<SheetEditReport, OfficeEngineError> {
        // 誤りはセッションを開く前に返す（kit を起こさない・文書に触れない）。
        let normalized = normalize(ops, default_sheet)?;
        let outcome = Box::pin(self.live.with_session(
            ctx,
            file_id,
            SessionAccess::Edit,
            SHEET_CONTENT_TYPES,
            async |client, _info| {
                let r = run::apply(client, label, &normalized).await;
                let modified = matches!(&r, Ok(o) if o.applied);
                (r, modified)
            },
        ))
        .await?;
        let applied = outcome.value?;
        Ok(SheetEditReport {
            file_name: outcome.info.file_name,
            text: applied.text,
            applied: applied.applied,
            save: outcome.save,
            views: outcome.info.views,
        })
    }
}
