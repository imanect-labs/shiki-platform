//! UI スペック検証層（Task 6.3・信頼境界）。
//!
//! LLM 出力・API 入力の生 JSON を**保存・描画の前段で必ず**検証する。
//! 違反は**拒否**（部分描画・暗黙補正で危険物を通さない）し、全件を
//! [`GuiValidationError`] で返す（LLM の自己修正・エディタ表示用）。
//!
//! 検証順: ①生 JSON の防御的上限（サイズ/深さ）→ ②serde パース（カタログ外・未知 props は
//! 型で表現不可能）→ ③意味検証（上限・アクション参照・URL スキーム・予約 variant 拒否）。
//! ワークフロー参照の存在・権限解決は非同期の [`SpecValidator`](crate::validator::SpecValidator)。

mod walk;

use serde::Serialize;
use ts_rs::TS;
use utoipa::ToSchema;

use crate::action::{ActionBinding, ALLOWED_ACTION_TOOLS};
use crate::spec::UiSpecDoc;

use walk::Walk;

/// 検証エラー（コード＋メッセージ＋位置）。workflow の `ValidationError` と同型の設計。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS, ToSchema)]
#[ts(export)]
pub struct GuiValidationError {
    /// エラーコード（例: `gui.unknown_action_ref`）。
    pub code: String,
    /// 人向け（かつ LLM の自己修正向け）メッセージ。
    pub message: String,
    /// 紐付くツリー位置（例: `root.children[2].on_click`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl GuiValidationError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        GuiValidationError {
            code: code.into(),
            message: message.into(),
            path: None,
        }
    }

    #[must_use]
    pub fn at(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }
}

/// 検証の上限（信頼境界の防御的リミット）。値の根拠は「チャット内 UI として十分・
/// DoS/描画コスト暴走を防ぐ」で、超える用途はミニアプリ側の分割を促す。
pub mod limits {
    /// スペック直列化サイズ上限（バイト）。artifact 全体の 1MiB より狭く取る。
    pub const MAX_SPEC_BYTES: usize = 256 * 1024;
    /// 生 JSON の最大ネスト深さ（パーサ保護・コンポーネント深さとは別）。
    pub const MAX_RAW_DEPTH: usize = 32;
    /// コンポーネントツリーの最大深さ。
    pub const MAX_DEPTH: usize = 8;
    /// コンポーネント総数上限。
    pub const MAX_NODES: usize = 200;
    /// アクション束縛数上限。
    pub const MAX_ACTIONS: usize = 20;
    /// コンテナ直下の子要素数上限。
    pub const MAX_CHILDREN: usize = 50;
    /// フォームのフィールド数上限。
    pub const MAX_FORM_FIELDS: usize = 50;
    /// select の選択肢数上限。
    pub const MAX_SELECT_OPTIONS: usize = 100;
    /// テーブル列数/行数上限。
    pub const MAX_TABLE_COLS: usize = 50;
    pub const MAX_TABLE_ROWS: usize = 500;
    /// チャートのデータ点上限。
    pub const MAX_CHART_POINTS: usize = 1000;
    /// `combo` の line 指定系列名の数上限。
    pub const MAX_LINE_SERIES: usize = 50;
    /// スタットタイルの sparkline 値数上限。
    pub const MAX_SPARKLINE_POINTS: usize = 200;
    /// コードブロックの文字数上限（表示スニペット想定）。
    pub const MAX_CODE_CHARS: usize = 8000;
    /// 本文テキスト（text/セル/default 値）の文字数上限。
    pub const MAX_TEXT_CHARS: usize = 4000;
    /// ラベル類（label/title/placeholder/x など）の文字数上限。
    pub const MAX_LABEL_CHARS: usize = 200;
    /// URL の文字数上限。
    pub const MAX_URL_CHARS: usize = 2048;
    /// id（action/form/field）の文字数上限。
    pub const MAX_ID_CHARS: usize = 64;
}

/// 生 JSON を検証し、型付きの [`UiSpecDoc`] を返す（同期・純粋）。
///
/// ワークフロー参照の解決（存在・権限・バージョンピン）は含まない —
/// 呼び出し面は必ず [`SpecValidator`](crate::validator::SpecValidator) を使うこと。
pub fn validate_spec(raw: &serde_json::Value) -> Result<UiSpecDoc, Vec<GuiValidationError>> {
    // ① 防御的上限（パース前）。直列化に失敗した値は fail-closed で拒否する
    // （0 扱いにするとサイズ検証が素通りし「拒否のみ」の契約と矛盾する）。
    let bytes = match serde_json::to_vec(raw) {
        Ok(v) => v.len(),
        Err(e) => {
            return Err(vec![GuiValidationError::new(
                "gui.schema_violation",
                format!("スペックを直列化できません: {e}"),
            )]);
        }
    };
    if bytes > limits::MAX_SPEC_BYTES {
        return Err(vec![GuiValidationError::new(
            "gui.spec_too_large",
            format!(
                "スペックが大きすぎます（{bytes} bytes > {} bytes）",
                limits::MAX_SPEC_BYTES
            ),
        )]);
    }
    if raw_depth(raw) > limits::MAX_RAW_DEPTH {
        return Err(vec![GuiValidationError::new(
            "gui.too_deep",
            format!(
                "JSON のネストが深すぎます（最大 {}）",
                limits::MAX_RAW_DEPTH
            ),
        )]);
    }

    // ② serde パース（カタログ外コンポーネント・未知 props は型で表現不可能＝ここで拒否）。
    let deserializer = raw.clone();
    let doc: UiSpecDoc = match serde_path_to_error::deserialize(deserializer) {
        Ok(doc) => doc,
        Err(e) => {
            let path = e.path().to_string();
            let msg = e.inner().to_string();
            let code = if msg.contains("unknown variant") {
                "gui.unknown_component"
            } else if msg.contains("unknown field") {
                "gui.unknown_prop"
            } else {
                "gui.schema_violation"
            };
            let err = GuiValidationError::new(code, msg);
            return Err(vec![if path.is_empty() || path == "." {
                err
            } else {
                err.at(path)
            }]);
        }
    };

    // ③ 意味検証（全件収集）。
    let mut errors = Vec::new();
    if doc.version != 1 {
        errors.push(GuiValidationError::new(
            "gui.unsupported_version",
            format!("version={} は未対応です（1 のみ）", doc.version),
        ));
    }
    validate_actions(&doc.actions, &mut errors);
    let action_ids: Vec<&str> = doc.actions.iter().map(ActionBinding::id).collect();
    let mut walk = Walk::new(&mut errors, &action_ids);
    walk.node(&doc.root, "root", 1);

    if errors.is_empty() {
        Ok(doc)
    } else {
        Err(errors)
    }
}

/// アクション束縛の検証（id 一意・閉語彙・ツール許可リスト）。
fn validate_actions(actions: &[ActionBinding], errors: &mut Vec<GuiValidationError>) {
    if actions.len() > limits::MAX_ACTIONS {
        errors.push(GuiValidationError::new(
            "gui.too_many_actions",
            format!("アクションが多すぎます（最大 {}）", limits::MAX_ACTIONS),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for (i, binding) in actions.iter().enumerate() {
        let path = format!("actions[{i}]");
        check_id(binding.id(), &path, errors);
        if !seen.insert(binding.id()) {
            errors.push(
                GuiValidationError::new(
                    "gui.duplicate_action_id",
                    format!("アクション id '{}' が重複しています", binding.id()),
                )
                .at(&path),
            );
        }
        match binding {
            ActionBinding::Tool(b) => {
                if !ALLOWED_ACTION_TOOLS.contains(&b.tool) {
                    errors.push(
                        GuiValidationError::new(
                            "gui.action_tool_forbidden",
                            format!(
                                "ツール '{}' は UI アクションに束縛できません（許可: {}）",
                                b.tool.as_str(),
                                ALLOWED_ACTION_TOOLS
                                    .iter()
                                    .map(|t| t.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        )
                        .at(&path),
                    );
                }
            }
            ActionBinding::Handler(_) => {} // HandlerKind は閉語彙（serde で保証）
            ActionBinding::Workflow(b) => {
                let has_name = b
                    .workflow
                    .name
                    .as_deref()
                    .is_some_and(|n| !n.trim().is_empty());
                if !has_name && b.workflow.artifact_id.is_none() {
                    errors.push(
                        GuiValidationError::new(
                            "gui.action_workflow_invalid",
                            "workflow 束縛には name か artifact_id が必要です",
                        )
                        .at(&path),
                    );
                }
                if b.workflow.version.is_some_and(|v| v < 1) {
                    errors.push(
                        GuiValidationError::new(
                            "gui.action_workflow_invalid",
                            "version は 1 以上を指定してください",
                        )
                        .at(&path),
                    );
                }
            }
        }
    }
}

/// id（action/form/field）の形式検証（空・長すぎ・記号を拒否）。
fn check_id(id: &str, path: &str, errors: &mut Vec<GuiValidationError>) {
    let valid = !id.is_empty()
        && id.chars().count() <= limits::MAX_ID_CHARS
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !valid {
        errors.push(
            GuiValidationError::new(
                "gui.invalid_id",
                format!(
                    "id '{id}' が不正です（英数と _ - のみ・最大 {} 文字）",
                    { limits::MAX_ID_CHARS }
                ),
            )
            .at(path),
        );
    }
}

/// 生 JSON のネスト深さ（反復・スタックセーフ）。
fn raw_depth(v: &serde_json::Value) -> usize {
    let mut max = 1;
    let mut stack: Vec<(&serde_json::Value, usize)> = vec![(v, 1)];
    while let Some((v, d)) = stack.pop() {
        max = max.max(d);
        if d > limits::MAX_RAW_DEPTH {
            return d; // 早期打ち切り（十分深い）
        }
        match v {
            serde_json::Value::Array(items) => stack.extend(items.iter().map(|i| (i, d + 1))),
            serde_json::Value::Object(map) => stack.extend(map.values().map(|i| (i, d + 1))),
            _ => {}
        }
    }
    max
}
