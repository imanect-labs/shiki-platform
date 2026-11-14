//! 会話の AI がテーブルと保存ビューを作る・直す道具（D-10 の T05・T06・GAP-32）。
//!
//! - [`DataTableCreateTool`]（`data_table_create`）: テーブルを作る。列・必須・選択肢・行の決まり・列の見せ方・
//!   状態の進め方（FSM）を 1 回で決める。**承認カードを経る**（HD-1 の改訂 (c)・テーブルには版が無く改訂は
//!   足すだけなので、作る前に列と決まりを人が確かめる）。オートの自律では自動で承認する（csv.write と同じ・削除で戻せる）。
//! - [`DataTableUpdateTool`]（`data_table_update`）: 自分が持ち主のテーブルを直す。承認カードを経る（行の決まり・
//!   列の見せ方・状態の進め方の進められる人は、権限の意味を変える）。オートでも承認を残す。
//! - [`DataViewSaveTool`]（`data_view_save`）: 保存ビューを作る・直す（確認なし・版を積む・T06）。
//!
//! どれも話した本人の `AuthContext` のまま data のチョークポイント（[`DataStore`]・[`FsmStore`]）を通す
//! （昇格しない）。テーブルの持ち主は作った本人だけで、共有（権限の付与）はしない（人が画面で行う）。
//!
//! - **対称のリンクは作らない**（R6）: 参照先のテーブルのスキーマを書き換えるため（共有中なら他人に効く）。
//!   片方向のリンク（`record_ref`）は受ける（参照先の閲覧の権限は data が確かめる）。
//! - **作成の冪等**（R1）: 本人がこの会話の run の間に同じ名前のテーブルを作っていれば、作り直さない（同じ内容なら
//!   作成済みとして返し、違えば直し方を言う）。ほかの人の同名・前からある同名は、名前が使えないとだけ言う。
//! - **上限**: 1 回の会話の run で作れるテーブルは [`MAX_TABLES_PER_RUN`] まで（自己修正のループで作り続けない）。
//! - **観測**: 成功した観測の末尾に参照の行（`参照: {...}`）を置き、`builder_refs` で参照カードを出す（T02）。
//! - **語彙は生成する**: 列の型の列挙は [`data::FieldType::ALL`] から作る。行の決まりの例はテストで data の検証に
//!   通して固定する（[`spec`] のテスト）。

mod spec;
mod table_create;
mod table_update;
mod view_save;

use std::sync::Arc;

use agent_core::ToolOutcome;
use authz::AuthContext;
use chrono::{DateTime, Utc};
use data::{DataError, DataStore, DataTable, FsmStore};
use uuid::Uuid;

pub use table_create::DataTableCreateTool;
pub use table_update::DataTableUpdateTool;
pub use view_save::{DataViewSaveTool, ViewBuilderDeps, MAX_VIEWS_PER_RUN};

/// 1 回の会話の run で作れるテーブルの数。
pub const MAX_TABLES_PER_RUN: usize = 5;

/// 道具が通るチョークポイント（どれも話した本人の権限で通す）。
#[derive(Clone)]
pub struct DataBuilderDeps {
    pub data: DataStore,
    pub fsms: Arc<FsmStore>,
    /// 状態の進め方を変える前に、承認を待っているレコードの数を数える（テーブルの設定の画面と同じ・GAP-44）。
    pub approvals: Arc<workflow_engine::ApprovalStore>,
}

/// 道具を組む会話の run（作成の冪等と上限の起点）。
#[derive(Debug, Clone, Copy)]
pub struct BuilderRun {
    /// 会話の run が作られた時刻（再試行しても変わらない）。
    pub since: DateTime<Utc>,
}

/// テーブルの指定（id か名前）を、閲覧できるテーブルへ。見えない・無いは同じ文言（存在を明かさない）。
async fn resolve_table(
    data: &DataStore,
    ctx: &AuthContext,
    table: &str,
    trace_id: Option<&str>,
) -> Result<DataTable, ToolOutcome> {
    let t = table.trim();
    let found = match Uuid::parse_str(t) {
        Ok(id) => data.get_table(ctx, id, trace_id).await,
        Err(_) => data.find_table_by_name(ctx, t, trace_id).await,
    };
    found.map_err(|e| failure(&e))
}

/// data の誤りを観測へ（見えない・無い・権限が無いは区別しない・data_record_write と同じ言い方）。
fn failure(err: &DataError) -> ToolOutcome {
    match err {
        DataError::NotFound | DataError::Forbidden => ToolOutcome::error(
            "テーブルが見つからないか、権限がありません。テーブルを直せるのは、作った人（持ち主）だけです。",
        ),
        _ => crate::data_tool::denied(err),
    }
}

/// 観測に載せる名前（1 行・上限つき）。
fn shown(name: &str) -> String {
    crate::data_tool::one_line(name, 60)
}

#[cfg(test)]
mod tests;
