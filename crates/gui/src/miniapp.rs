//! ミニアプリの body スキーマ＋検証（Task 6.10 / miniapp-platform §6・#604）。
//!
//! ミニアプリ = 画面（Yui の ```ui ブロックを含む Markdown）＋ skill ＋ ワークフローの
//! **バージョン固定参照**を束ねたアーティファクト（kind=mini_app）。画面のボタンの実行先
//! （`-> workflow 名前`）は、束ねたワークフローの別名からだけ選べる。テーブル（構造化データ）は
//! Phase 9 合流後に追加。
//! 部品は常に明示ピン（再現性）で、共有はミニアプリ本体の ReBAC が正
//! （部品の個別共有は不要 — 解決はバンドル権限チョークポイント
//! [`ArtifactStore::get_version_via_bundle`] を通る）。

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::validate::GuiValidationError;

/// ミニアプリが束ねられるワークフロー数の上限。
pub const MAX_WORKFLOWS: usize = 20;

/// 画面（Markdown）の大きさの上限（バイト）。
pub const MAX_SCREEN_BYTES: usize = 64 * 1024;

/// 部品への固定参照（常に明示ピン）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ComponentPin {
    pub artifact_id: Uuid,
    /// JSON 経由の数値のため TS では number（bigint にしない）。
    #[ts(type = "number")]
    pub version: i64,
}

/// 名前付きワークフロー参照（画面のボタンの `-> workflow 別名` で参照する）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct NamedComponentPin {
    /// 画面のボタンが参照する別名（ミニアプリ内で一意・前後に空白を含まない）。
    pub alias: String,
    pub artifact_id: Uuid,
    /// JSON 経由の数値のため TS では number（bigint にしない）。
    #[ts(type = "number")]
    pub version: i64,
}

/// ミニアプリ本文（artifact kind=mini_app の body JSONB）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MiniAppBody {
    pub description: String,
    /// 画面（Yui の ```ui ブロックを含む Markdown）。ボタンを持てるのはトップレベルの `app`
    /// だけで、実行先は `workflows` の別名に限る。
    pub screen: String,
    /// 初期コンテキストに適用する skill（kind=skill・任意）。
    #[serde(default)]
    pub skill: Option<ComponentPin>,
    /// 束ねるワークフロー（kind=workflow・画面のボタンの実行先はこの集合に限る）。
    #[serde(default)]
    pub workflows: Vec<NamedComponentPin>,
}

/// mini_app body の構造検証（同期・純粋）。参照の存在・kind・権限は
/// [`MiniAppStore`](crate::miniapp_store::MiniAppStore) が作成者権限で解決する。
pub fn validate_miniapp_body(
    raw: &serde_json::Value,
) -> Result<MiniAppBody, Vec<GuiValidationError>> {
    let body: MiniAppBody = match serde_path_to_error::deserialize(raw.clone()) {
        Ok(body) => body,
        Err(e) => {
            let path = e.path().to_string();
            let err = GuiValidationError::new("miniapp.schema_violation", e.inner().to_string());
            return Err(vec![if path.is_empty() || path == "." {
                err
            } else {
                err.at(path)
            }]);
        }
    };
    let mut errors = Vec::new();
    if body.description.trim().is_empty() {
        errors.push(GuiValidationError::new(
            "miniapp.empty_description",
            "description は必須です",
        ));
    }
    if body.workflows.len() > MAX_WORKFLOWS {
        errors.push(
            GuiValidationError::new(
                "miniapp.too_many_workflows",
                format!("workflows は最大 {MAX_WORKFLOWS} 件"),
            )
            .at("workflows"),
        );
    }
    let mut aliases = std::collections::HashSet::new();
    let mut ids = std::collections::HashSet::new();
    for (i, wf) in body.workflows.iter().enumerate() {
        let path = format!("workflows[{i}]");
        // ボタンの `-> workflow 名前` と同じく途中の空白は許す（前後の空白・制御文字は不可）。
        if wf.alias.trim().is_empty()
            || wf.alias.trim() != wf.alias
            || wf.alias.chars().count() > 64
            || wf.alias.chars().any(char::is_control)
        {
            errors.push(
                GuiValidationError::new(
                    "miniapp.invalid_alias",
                    "alias は前後に空白の無い 1〜64 文字（改行などの制御文字は不可）",
                )
                .at(&path),
            );
        }
        if !aliases.insert(wf.alias.clone()) {
            errors.push(
                GuiValidationError::new(
                    "miniapp.duplicate_alias",
                    format!("alias '{}' が重複しています", wf.alias),
                )
                .at(&path),
            );
        }
        if !ids.insert(wf.artifact_id) {
            errors.push(
                GuiValidationError::new(
                    "miniapp.duplicate_workflow",
                    "同じワークフローを複数回束ねています",
                )
                .at(&path),
            );
        }
        if wf.version < 1 {
            errors.push(
                GuiValidationError::new("miniapp.invalid_version", "version は 1 以上").at(&path),
            );
        }
    }
    check_screen(&body, &mut errors);
    if let Some(skill) = &body.skill {
        if skill.version < 1 {
            errors.push(
                GuiValidationError::new("miniapp.invalid_version", "version は 1 以上").at("skill"),
            );
        }
    }
    if errors.is_empty() {
        Ok(body)
    } else {
        Err(errors)
    }
}

/// 画面の検証: 大きさ、ボタンの種類と実行先（束ねたワークフローの別名だけ）。
///
/// 会話に送る種類（`ask`・`plan`・`-> chat`）は、会話の無いミニアプリでは押せないので拒む。
/// 読むのはボタンを持てるブロック（トップレベル・閉じている・エラーの無いもの）だけで、
/// 書き損じた図はボタンを持たず、表示もコードのままになる。
fn check_screen(body: &MiniAppBody, errors: &mut Vec<GuiValidationError>) {
    if body.screen.trim().is_empty() {
        errors.push(GuiValidationError::new("miniapp.empty_screen", "画面は必須です").at("screen"));
        return;
    }
    if body.screen.len() > MAX_SCREEN_BYTES {
        errors.push(
            GuiValidationError::new(
                "miniapp.screen_too_large",
                format!("画面は {} KB までです", MAX_SCREEN_BYTES / 1024),
            )
            .at("screen"),
        );
        return;
    }
    for block in yui::extract_action_blocks(&body.screen) {
        let title = match &block.figure {
            yui::Figure::App { base, .. }
            | yui::Figure::Ask { base, .. }
            | yui::Figure::Plan { base, .. } => base.title.clone(),
        };
        if !matches!(block.figure, yui::Figure::App { .. }) {
            errors.push(
                GuiValidationError::new(
                    "miniapp.chat_kind_not_supported",
                    format!("「{title}」: 質問（ask）と計画（plan）は会話でだけ使えます"),
                )
                .at("screen"),
            );
            continue;
        }
        for action in yui::figure_actions(&block.figure) {
            match &action.target {
                yui::ActionTarget::Chat => errors.push(
                    GuiValidationError::new(
                        "miniapp.chat_target_not_supported",
                        format!(
                            "ボタン「{}」: 会話に送るボタン（-> chat）はミニアプリでは使えません",
                            action.label
                        ),
                    )
                    .at("screen"),
                ),
                yui::ActionTarget::Workflow { name } => {
                    if !body.workflows.iter().any(|w| &w.alias == name) {
                        errors.push(
                            GuiValidationError::new(
                                "miniapp.workflow_not_bundled",
                                format!(
                                    "ボタン「{}」の実行先「{name}」が、束ねたワークフローにありません",
                                    action.label
                                ),
                            )
                            .at("screen"),
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body(screen: &str, aliases: &[&str]) -> serde_json::Value {
        json!({
            "description": "経費の申請",
            "screen": screen,
            "workflows": aliases.iter().enumerate().map(|(i, a)| json!({
                "alias": a,
                "artifact_id": uuid::Uuid::from_u128(i as u128 + 1),
                "version": 1
            })).collect::<Vec<_>>(),
        })
    }

    const APP: &str = "# 経費\n\n```ui\napp 申請\n金額 = number 必須 円 as amount\nbutton 申請する -> workflow expense\n```\n";

    fn codes(raw: &serde_json::Value) -> Vec<String> {
        validate_miniapp_body(raw)
            .err()
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.code)
            .collect()
    }

    #[test]
    fn accepts_buttons_that_target_bundled_workflows() {
        assert!(validate_miniapp_body(&body(APP, &["expense"])).is_ok());
        // 表示だけの図・本文だけの画面も通る。
        let plain = "```ui\nbar 売上\n月 | 額\n1月 | 10\n```";
        assert!(validate_miniapp_body(&body(plain, &[])).is_ok());
    }

    #[test]
    fn rejects_unbundled_and_chat_targets() {
        assert_eq!(
            codes(&body(APP, &["other"])),
            vec!["miniapp.workflow_not_bundled"]
        );
        let chat = "```ui\napp 相談\n内容 = textarea 必須\nbutton 送る -> chat\n```";
        assert_eq!(
            codes(&body(chat, &[])),
            vec!["miniapp.chat_target_not_supported"]
        );
        let ask = "```ui\nask 確認\n? 対象は\n- 国内\n- 海外\n```";
        assert_eq!(
            codes(&body(ask, &[])),
            vec!["miniapp.chat_kind_not_supported"]
        );
    }

    #[test]
    fn rejects_empty_or_huge_screens_and_bad_aliases() {
        assert_eq!(codes(&body("  ", &[])), vec!["miniapp.empty_screen"]);
        let huge = "あ".repeat(MAX_SCREEN_BYTES);
        assert_eq!(codes(&body(&huge, &[])), vec!["miniapp.screen_too_large"]);
        // 途中に空白のある名前（`-> workflow 経費 申請`）はそのまま別名にできる。
        let spaced = APP.replace("workflow expense", "workflow 経費 申請");
        assert!(codes(&body(&spaced, &["経費 申請"])).is_empty());
        assert!(codes(&body(APP, &[" expense"])).contains(&"miniapp.invalid_alias".to_string()));
        assert!(codes(&body(APP, &["exp\nense"])).contains(&"miniapp.invalid_alias".to_string()));
        // 旧形式（ui_spec）は受け付けない。
        let old = json!({ "description": "d", "ui_spec": { "artifact_id": uuid::Uuid::nil(), "version": 1 } });
        assert_eq!(codes(&old), vec!["miniapp.schema_violation"]);
    }
}
