//! 接続済みのセッションに対する Word の読み書き（#725）。

use std::fmt::Write as _;

use serde_json::json;

use super::normalize::DocNormalized;
use super::view;
use crate::engine::{call, EngineError};
use crate::live::CoolWsClient;

/// 概要で一度に並べるブロック数。
pub const OVERVIEW_BLOCKS: u32 = 200;

/// 文書の概要（ページ設定・スタイル・ブロック一覧）。`from` からのブロックを並べる。
pub async fn read_overview(
    client: &mut CoolWsClient,
    file_name: &str,
    from: u32,
) -> Result<String, EngineError> {
    let v = call(
        client,
        json!({"action": "writer.overview", "from": from, "limit": OVERVIEW_BLOCKS}),
    )
    .await?;
    Ok(view::overview_text(file_name, &v))
}

/// ブロックの範囲を、書式つきの HTML で読む。
pub async fn read_blocks(
    client: &mut CoolWsClient,
    from: &str,
    to: Option<&str>,
) -> Result<String, EngineError> {
    let v = call(
        client,
        json!({"action": "writer.read_blocks", "from": from, "to": to}),
    )
    .await?;
    Ok(view::blocks_text(&v))
}

/// 適用の結果。
#[derive(Debug, Clone)]
pub struct DocApplyOutcome {
    pub text: String,
    pub applied: bool,
}

/// 正規化済みの操作をまとめて適用し、文書の概要を読み戻す（新しいブロック ID を返すため）。
pub async fn apply(
    client: &mut CoolWsClient,
    label: &str,
    normalized: &DocNormalized,
) -> Result<DocApplyOutcome, EngineError> {
    let result = call(
        client,
        json!({"action": "writer.apply", "label": format!("Shiki AI: {label}"), "block_ids": normalized.block_ids, "ops": normalized.ops}),
    )
    .await;
    let results = match result {
        Ok(v) => v,
        Err(EngineError::Rejected {
            code,
            message,
            stage,
            op_index,
        }) => {
            let which = op_index
                .and_then(|i| normalized.origin.get(i))
                .map(|(n, name)| format!("{n} 件目の操作（{name}）"))
                .unwrap_or_default();
            let stage = if stage.contains("件目") || stage.is_empty() {
                String::new()
            } else {
                format!("・{stage}")
            };
            let mut text = format!(
                "適用できませんでした。途中までの変更もすべて取り消したので、文書は変わっていません。\n原因: {which}{stage}: {message}"
            );
            if code == "stale_block_id" {
                let _ = write!(
                    text,
                    "\nブロック ID は office.read で読み直して取り直してください。"
                );
            }
            return Ok(DocApplyOutcome {
                text,
                applied: false,
            });
        }
        Err(other) => return Err(other),
    };
    let mut text = format!(
        "{} 件の操作を適用しました（人は 1 回の「元に戻す」で取り消せます）。",
        results["results"].as_array().map_or(0, Vec::len)
    );
    for r in results["results"].as_array().into_iter().flatten() {
        if let Some(n) = r["replaced"].as_u64() {
            let _ = write!(text, "\n- replace_text: {n} か所を置き換えました");
        }
        if let Some(n) = r["formatted"].as_u64() {
            let _ = write!(text, "\n- format_text: {n} か所に書式を付けました");
        }
    }
    // ブロック ID は変わる（位置とハッシュ）。続けて編集できるよう、最新の一覧を返す。
    let overview = call(
        client,
        json!({"action": "writer.overview", "from": 0, "limit": OVERVIEW_BLOCKS}),
    )
    .await?;
    let _ = write!(
        text,
        "\n\n現在の文書（ブロック ID は更新されています。以降はこの ID を使う）:\n{}",
        view::overview_text("", &overview)
    );
    Ok(DocApplyOutcome {
        text,
        applied: true,
    })
}
