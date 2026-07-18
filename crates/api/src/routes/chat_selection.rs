//! 選択コンテキストの受理検証（選択→AI 指示・Task 11.10・design §4.8.3）。
//!
//! クライアント由来の `SelectionContext` は信用しない。`node_id` は**実行主体の viewer
//! 権限で再解決できた場合のみ**受理する（読めない/存在しない対象は fail-closed で
//! 404・存在秘匿）。`locator`/`excerpt` は表示・誘導用データであり権限の根拠にしない —
//! 編集ツール側が自身の editor 認可を通るため、SelectionContext は認可をバイパスしない。

use authz::AuthContext;
use chat::SelectionContext;

use crate::error::ApiError;
use crate::state::AppState;

pub(crate) async fn resolve_selection(
    state: &AppState,
    ctx: &AuthContext,
    context: Option<SelectionContext>,
    trace_id: Option<&str>,
) -> Result<Option<SelectionContext>, ApiError> {
    let Some(context) = context else {
        return Ok(None);
    };
    if let Some(node_id) = context.node_id {
        // viewer 判定＋監査は storage のチョークポイントに委ねる（get_metadata）。
        state.storage.get_metadata(ctx, node_id, trace_id).await?;
    }
    Ok(Some(context))
}
