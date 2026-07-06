//! サンドボックス成果物の回収（`/workspace` → [`ArtifactStore`]・Task 4.11）。
//!
//! 実行成功後に `/workspace` のファイルを列挙・取得し、発話ユーザー権限で保存する。
//! ゲスト由来のファイル名・内容は敵対的として扱う（PIT-23）: 名前はここで粗く弾き、最終検証は
//! ストレージ側（`validate_name`）が単一チョークポイントとして行う。回収失敗はツール全体を
//! 失敗させず、観測テキストに注記してモデルに回復させる。

use authz::AuthContext;
use sandbox_client::{Sandbox, SandboxHandle};

use crate::tool::{ArtifactStore, ToolOutcome};

/// 回収する成果物の上限（orchestrator 側の validate と同値・二重防御）。
const ARTIFACT_MAX_COUNT: usize = 20;
/// 成果物 1 個のサイズ上限（orchestrator 側の 8MiB と同値・二重防御）。
const ARTIFACT_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// 実行コードを書き込む guest パス（orchestrator の Python 実行が置く・成果物から除外）。
const ENTRYPOINT_NAME: &str = "main.py";

/// `/workspace` の成果物を回収して保存し、`out` に参照と注記を書き足す。
pub(super) async fn collect_artifacts(
    sandbox: &dyn Sandbox,
    ctx: &AuthContext,
    handle: &SandboxHandle,
    store: &dyn ArtifactStore,
    out: &mut ToolOutcome,
    trace_id: Option<&str>,
) {
    use std::fmt::Write as _;
    let entries = match sandbox.list_dir(handle, "/workspace").await {
        Ok(entries) => entries,
        Err(e) => {
            let _ = write!(out.content, "\n（成果物の一覧取得に失敗: {e}）\n");
            return;
        }
    };
    let mut saved = Vec::new();
    let mut notes = Vec::new();
    for entry in entries {
        if saved.len() >= ARTIFACT_MAX_COUNT {
            notes.push(format!(
                "成果物が {ARTIFACT_MAX_COUNT} 個を超えたため以降は保存しませんでした"
            ));
            break;
        }
        // ディレクトリ・実行コード本体・サブパス（ゲストが細工した名前）は対象外。
        if entry.is_dir || entry.name == ENTRYPOINT_NAME || entry.name.contains('/') {
            continue;
        }
        if entry.size > ARTIFACT_MAX_BYTES {
            notes.push(format!(
                "{} はサイズ上限（{ARTIFACT_MAX_BYTES} バイト）を超えるため保存しませんでした",
                entry.name
            ));
            continue;
        }
        let bytes = match sandbox
            .get_file(handle, &format!("/workspace/{}", entry.name))
            .await
        {
            Ok(bytes) => bytes,
            Err(e) => {
                notes.push(format!("{} の取得に失敗: {e}", entry.name));
                continue;
            }
        };
        match store
            .save(
                ctx,
                &entry.name,
                bytes,
                content_type_for(&entry.name),
                trace_id,
            )
            .await
        {
            Ok(artifact) => saved.push(artifact),
            Err(e) => notes.push(format!("{} の保存に失敗: {e}", entry.name)),
        }
    }
    if !saved.is_empty() {
        out.content.push_str("\n保存した成果物:\n");
        for a in &saved {
            let _ = writeln!(out.content, "- {} (node_id: {})", a.name, a.node_id);
        }
    }
    for note in notes {
        let _ = writeln!(out.content, "（{note}）");
    }
    out.artifacts = saved;
}

/// 拡張子から content_type を推定する（成果物保存用の最小マップ）。
fn content_type_for(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or_default() {
        "csv" => "text/csv",
        "json" => "application/json",
        "txt" => "text/plain",
        "md" => "text/markdown",
        "html" => "text/html",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn content_type_covers_common_extensions() {
        assert_eq!(content_type_for("a.csv"), "text/csv");
        assert_eq!(content_type_for("a.json"), "application/json");
        assert_eq!(content_type_for("report.pdf"), "application/pdf");
        assert_eq!(content_type_for("noext"), "application/octet-stream");
    }
}
