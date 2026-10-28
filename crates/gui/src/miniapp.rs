//! ミニアプリの body スキーマ＋検証（Task 6.10 / miniapp-platform §6・#604）。
//!
//! ミニアプリ = 画面（Yui の ```ui ブロックを含む Markdown）＋ skill ＋ ワークフローの
//! **バージョン固定参照**を束ねたアーティファクト（kind=mini_app）。画面のボタンの実行先
//! （`-> workflow 名前`）は、束ねたワークフローの別名からだけ選べる。テーブル（構造化データ）は
//! 保存ビューのピン（`data_views`）を画面の ```data_grid で参照して描く（Task 15.7・[`crate::data_grid`]）。
//! 部品は常に明示ピン（再現性）で、共有はミニアプリ本体の ReBAC が正
//! （部品の個別共有は不要 — 解決はバンドル権限チョークポイント
//! [`ArtifactStore::get_version_via_bundle`] を通る）。

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::validate::GuiValidationError;

/// ミニアプリが束ねられるワークフロー数の上限。
pub const MAX_WORKFLOWS: usize = 20;

/// ミニアプリが束ねられる保存ビュー数の上限。
pub const MAX_DATA_VIEWS: usize = 20;

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
    /// 束ねる保存ビュー（kind=data_view・画面の ```data_grid が参照できるのはこの集合に限る・Task 15.7）。
    ///
    /// 定義（クエリ）は保存した版に固定し、**データは閲覧者本人の権限で毎回評価する**
    /// （[`ActionBinding::DataView`](crate::ActionBinding::DataView)）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<NamedComponentPin>>")]
    pub data_views: Vec<NamedComponentPin>,
}

/// 名前付きピンの集合ごとの検証の語彙（ワークフロー・保存ビュー）。
struct PinSet {
    /// body のフィールド名（エラー位置の接頭辞）。
    field: &'static str,
    max: usize,
    too_many: &'static str,
    duplicate: &'static str,
    /// 重複のメッセージの名詞（「同じ〇〇を複数回束ねています」）。
    noun: &'static str,
}

impl PinSet {
    const WORKFLOWS: PinSet = PinSet {
        field: "workflows",
        max: MAX_WORKFLOWS,
        too_many: "miniapp.too_many_workflows",
        duplicate: "miniapp.duplicate_workflow",
        noun: "ワークフロー",
    };
    const DATA_VIEWS: PinSet = PinSet {
        field: "data_views",
        max: MAX_DATA_VIEWS,
        too_many: "miniapp.too_many_data_views",
        duplicate: "miniapp.duplicate_data_view",
        noun: "保存ビュー",
    };
}

/// 名前付きピン（別名・重複・版）の構造検証。
fn check_named_pins(
    pins: &[NamedComponentPin],
    set: &PinSet,
    errors: &mut Vec<GuiValidationError>,
) {
    if pins.len() > set.max {
        errors.push(
            GuiValidationError::new(set.too_many, format!("{} は最大 {} 件", set.field, set.max))
                .at(set.field),
        );
    }
    let mut aliases = std::collections::HashSet::new();
    let mut ids = std::collections::HashSet::new();
    for (i, pin) in pins.iter().enumerate() {
        let path = format!("{}[{i}]", set.field);
        // ボタンの `-> workflow 名前` と同じく途中の空白は許す（前後の空白・制御文字は不可）。
        if pin.alias.trim().is_empty()
            || pin.alias.trim() != pin.alias
            || pin.alias.chars().count() > 64
            || pin.alias.chars().any(char::is_control)
        {
            errors.push(
                GuiValidationError::new(
                    "miniapp.invalid_alias",
                    "alias は前後に空白の無い 1〜64 文字（改行などの制御文字は不可）",
                )
                .at(&path),
            );
        }
        if !aliases.insert(pin.alias.clone()) {
            errors.push(
                GuiValidationError::new(
                    "miniapp.duplicate_alias",
                    format!("alias '{}' が重複しています", pin.alias),
                )
                .at(&path),
            );
        }
        if !ids.insert(pin.artifact_id) {
            errors.push(
                GuiValidationError::new(
                    set.duplicate,
                    format!("同じ{}を複数回束ねています", set.noun),
                )
                .at(&path),
            );
        }
        if pin.version < 1 {
            errors.push(
                GuiValidationError::new("miniapp.invalid_version", "version は 1 以上").at(&path),
            );
        }
    }
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
    check_named_pins(&body.workflows, &PinSet::WORKFLOWS, &mut errors);
    check_named_pins(&body.data_views, &PinSet::DATA_VIEWS, &mut errors);
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
    for grid in crate::data_grid::data_grid_blocks(&body.screen) {
        match grid.alias.as_deref() {
            Some(alias) if body.data_views.iter().any(|v| v.alias == alias) => {}
            Some(alias) => errors.push(
                GuiValidationError::new(
                    "miniapp.data_view_not_bundled",
                    format!("表「{alias}」が、束ねた保存ビューにありません"),
                )
                .at("screen"),
            ),
            None => errors.push(
                GuiValidationError::new(
                    "miniapp.data_grid_without_view",
                    "```data_grid には束ねた保存ビューの別名を 1 行で書いてください",
                )
                .at("screen"),
            ),
        }
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
