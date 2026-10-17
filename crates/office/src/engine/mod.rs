//! Shiki Office Engine（#725・design §4.8）。
//!
//! AI が Word/Excel を「レイアウト水準で何でも」操作するための層。kit（LibreOffice）の中で
//! 走る**固定の JS インタプリタ**（`crates/office/engine/*.js`）へ、検証・正規化済みの JSON を
//! `executescript` で渡す（PIT-68 ⑤: LLM のコードは kit に入れない）。
//!
//! - `a1` / `formula`: Excel の書き方（A1 表記・数式）の解釈と Calc の API 文法への変換
//! - `calc`: Excel の操作語彙・正規化・読み取りの整形・実行
//! - `script`: 固定ソースの組み立て
//! - `service`: セッション（認可・直列化・保存）に包んだ入口 [`OfficeEngine`]

pub mod a1;
pub mod calc;
pub mod formula;
pub mod script;
mod service;
pub mod writer;

pub use service::{
    OfficeEngine, OfficeEngineError, SheetEditReport, SheetReadRequest, DOC_CONTENT_TYPES,
    SHEET_CONTENT_TYPES,
};

use serde_json::Value;

use crate::live::{CoolWsClient, LiveError, ScriptResult};

/// エンジン呼び出しの失敗。
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// セッション（WS）の失敗。
    #[error(transparent)]
    Session(#[from] LiveError),
    /// kit 側の例外（構文エラー・未捕捉）。エンジンの不具合なのでログに残す。
    #[error("文書エンジンが異常終了しました: {0}")]
    Script(String),
    /// エンジンが操作を拒否した（変更は取り消し済み）。
    #[error("{message}")]
    Rejected {
        code: String,
        message: String,
        stage: String,
        op_index: Option<usize>,
    },
}

/// エンジンの action を 1 回呼ぶ。`{ok: true, result}` の result を返す。
pub(crate) async fn call(client: &mut CoolWsClient, req: Value) -> Result<Value, EngineError> {
    match client
        .execute_script(&script::ENGINE_SOURCE, &Value::Array(vec![req]))
        .await?
    {
        ScriptResult::Ok(v) if v["ok"] == Value::Bool(true) => Ok(v["result"].clone()),
        ScriptResult::Ok(v) => {
            let e = &v["error"];
            Err(EngineError::Rejected {
                code: e["code"].as_str().unwrap_or("unknown").to_string(),
                message: e["message"]
                    .as_str()
                    .unwrap_or("操作に失敗しました")
                    .to_string(),
                stage: e["stage"].as_str().unwrap_or("").to_string(),
                op_index: e["op_index"].as_u64().and_then(|i| usize::try_from(i).ok()),
            })
        }
        ScriptResult::Err(e) => {
            tracing::error!(error = %e, "office engine: kit 内のスクリプトが例外で終了");
            Err(EngineError::Script(e))
        }
    }
}
