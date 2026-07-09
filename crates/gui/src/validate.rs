//! UI スペック検証層（Task 6.3・信頼境界）。
//!
//! LLM 出力・API 入力の生 JSON を**保存・描画の前段で必ず**検証する。
//! 違反は**拒否**（部分描画・暗黙補正で危険物を通さない）し、全件を
//! [`GuiValidationError`] で返す（LLM の自己修正・エディタ表示用）。
//!
//! 検証順: ①生 JSON の防御的上限（サイズ/深さ）→ ②serde パース（カタログ外・未知 props は
//! 型で表現不可能）→ ③意味検証（上限・アクション参照・URL スキーム・予約 variant 拒否）。
//! ワークフロー参照の存在・権限解決は非同期の [`SpecValidator`](crate::validator::SpecValidator)。

use serde::Serialize;
use ts_rs::TS;
use utoipa::ToSchema;

use crate::action::{ActionBinding, ALLOWED_ACTION_TOOLS};
use crate::spec::{ActionRef, FormField, UiNode, UiSpecDoc};

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
    // ① 防御的上限（パース前）。
    let bytes = serde_json::to_vec(raw).map_or(0, |v| v.len());
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
    let mut walk = Walk {
        errors: &mut errors,
        action_ids: &action_ids,
        node_count: 0,
        node_overflow: false,
        form_ids: Vec::new(),
    };
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

/// ツリー走査の状態。
struct Walk<'a> {
    errors: &'a mut Vec<GuiValidationError>,
    action_ids: &'a [&'a str],
    node_count: usize,
    node_overflow: bool,
    form_ids: Vec<String>,
}

impl Walk<'_> {
    #[allow(clippy::too_many_lines)] // コンポーネント別の検証分岐（分割すると対応が読みにくい）。
    fn node(&mut self, node: &UiNode, path: &str, depth: usize) {
        self.node_count += 1;
        if self.node_count > limits::MAX_NODES && !self.node_overflow {
            self.node_overflow = true;
            self.errors.push(GuiValidationError::new(
                "gui.too_many_nodes",
                format!("コンポーネントが多すぎます（最大 {}）", limits::MAX_NODES),
            ));
        }
        if depth > limits::MAX_DEPTH {
            self.errors.push(
                GuiValidationError::new(
                    "gui.too_deep",
                    format!("ネストが深すぎます（最大 {}）", limits::MAX_DEPTH),
                )
                .at(path),
            );
            return; // これ以上潜らない（エラーの洪水を防ぐ）
        }
        if !node.kind().available() {
            self.errors.push(
                GuiValidationError::new(
                    "gui.component_unavailable",
                    format!(
                        "コンポーネント '{}' は予約済みで未対応です",
                        node.kind().as_str()
                    ),
                )
                .at(path),
            );
            return;
        }
        match node {
            UiNode::Container(p) => {
                self.opt_label(p.title.as_deref(), &format!("{path}.title"));
                if p.children.len() > limits::MAX_CHILDREN {
                    self.errors.push(
                        GuiValidationError::new(
                            "gui.too_many_children",
                            format!("子要素が多すぎます（最大 {}）", limits::MAX_CHILDREN),
                        )
                        .at(path),
                    );
                }
                for (i, child) in p.children.iter().enumerate() {
                    self.node(child, &format!("{path}.children[{i}]"), depth + 1);
                }
            }
            UiNode::Text(p) => {
                self.text(&p.text, limits::MAX_TEXT_CHARS, &format!("{path}.text"));
            }
            UiNode::Link(p) => {
                self.label(&p.text, &format!("{path}.text"));
                let href_path = format!("{path}.href");
                if p.href.chars().count() > limits::MAX_URL_CHARS {
                    self.errors.push(
                        GuiValidationError::new("gui.string_too_long", "URL が長すぎます")
                            .at(&href_path),
                    );
                }
                // https のみ（javascript:/data:/相対 URL を構造的に拒否）。補正はしない。
                if !p.href.starts_with("https://") {
                    self.errors.push(
                        GuiValidationError::new(
                            "gui.forbidden_url_scheme",
                            "リンクは https:// のみ使用できます",
                        )
                        .at(&href_path),
                    );
                }
            }
            UiNode::Form(p) => self.form(p, path),
            UiNode::Button(p) => {
                self.label(&p.label, &format!("{path}.label"));
                self.action_ref(&p.on_click, &format!("{path}.on_click"));
            }
            UiNode::Table(p) => self.table(p, path),
            UiNode::Chart(spec) => {
                self.opt_label(spec.title.as_deref(), &format!("{path}.title"));
                self.opt_label(spec.x_label.as_deref(), &format!("{path}.x_label"));
                self.opt_label(spec.y_label.as_deref(), &format!("{path}.y_label"));
                if spec.data.len() > limits::MAX_CHART_POINTS {
                    self.errors.push(
                        GuiValidationError::new(
                            "gui.too_many_points",
                            format!("データ点が多すぎます（最大 {}）", limits::MAX_CHART_POINTS),
                        )
                        .at(path),
                    );
                }
                for (i, point) in spec.data.iter().enumerate() {
                    self.label(&point.x, &format!("{path}.data[{i}].x"));
                    self.opt_label(point.series.as_deref(), &format!("{path}.data[{i}].series"));
                    if !point.y.is_finite() {
                        self.errors.push(
                            GuiValidationError::new("gui.invalid_number", "y は有限数のみ")
                                .at(format!("{path}.data[{i}].y")),
                        );
                    }
                }
            }
            // available() 判定で早期 return 済み。
            UiNode::Map(_) | UiNode::Image(_) => unreachable!("reserved components return early"),
        }
    }

    fn form(&mut self, p: &crate::spec::FormProps, path: &str) {
        check_id(&p.id, &format!("{path}.id"), self.errors);
        if self.form_ids.iter().any(|id| id == &p.id) {
            self.errors.push(
                GuiValidationError::new(
                    "gui.duplicate_form_id",
                    format!("フォーム id '{}' が重複しています", p.id),
                )
                .at(path),
            );
        }
        self.form_ids.push(p.id.clone());
        self.opt_label(p.title.as_deref(), &format!("{path}.title"));
        self.opt_label(p.submit_label.as_deref(), &format!("{path}.submit_label"));
        self.action_ref(&p.submit, &format!("{path}.submit"));
        if p.fields.len() > limits::MAX_FORM_FIELDS {
            self.errors.push(
                GuiValidationError::new(
                    "gui.too_many_fields",
                    format!("フィールドが多すぎます（最大 {}）", limits::MAX_FORM_FIELDS),
                )
                .at(path),
            );
        }
        let mut seen = std::collections::HashSet::new();
        for (i, field) in p.fields.iter().enumerate() {
            let fpath = format!("{path}.fields[{i}]");
            check_id(field.id(), &fpath, self.errors);
            if !seen.insert(field.id().to_string()) {
                self.errors.push(
                    GuiValidationError::new(
                        "gui.duplicate_field_id",
                        format!("フィールド id '{}' が重複しています", field.id()),
                    )
                    .at(&fpath),
                );
            }
            match field {
                FormField::TextInput(f) => {
                    self.label(&f.label, &format!("{fpath}.label"));
                    self.opt_label(f.placeholder.as_deref(), &format!("{fpath}.placeholder"));
                    if let Some(d) = &f.default {
                        self.text(d, limits::MAX_TEXT_CHARS, &format!("{fpath}.default"));
                    }
                }
                FormField::Select(f) => {
                    self.label(&f.label, &format!("{fpath}.label"));
                    if f.options.len() > limits::MAX_SELECT_OPTIONS {
                        self.errors.push(
                            GuiValidationError::new(
                                "gui.too_many_options",
                                format!(
                                    "選択肢が多すぎます（最大 {}）",
                                    limits::MAX_SELECT_OPTIONS
                                ),
                            )
                            .at(&fpath),
                        );
                    }
                    for (j, opt) in f.options.iter().enumerate() {
                        self.label(&opt.value, &format!("{fpath}.options[{j}].value"));
                        self.label(&opt.label, &format!("{fpath}.options[{j}].label"));
                    }
                    if let Some(d) = &f.default {
                        if !f.options.iter().any(|o| &o.value == d) {
                            self.errors.push(
                                GuiValidationError::new(
                                    "gui.invalid_default",
                                    "default は options の value から選んでください",
                                )
                                .at(format!("{fpath}.default")),
                            );
                        }
                    }
                }
            }
        }
    }

    fn table(&mut self, p: &crate::spec::TableProps, path: &str) {
        self.opt_label(p.title.as_deref(), &format!("{path}.title"));
        if p.columns.is_empty() {
            self.errors.push(
                GuiValidationError::new("gui.empty_table", "columns は 1 列以上必要です").at(path),
            );
        }
        if p.columns.len() > limits::MAX_TABLE_COLS {
            self.errors.push(
                GuiValidationError::new(
                    "gui.too_many_columns",
                    format!("列が多すぎます（最大 {}）", limits::MAX_TABLE_COLS),
                )
                .at(path),
            );
        }
        if p.rows.len() > limits::MAX_TABLE_ROWS {
            self.errors.push(
                GuiValidationError::new(
                    "gui.too_many_rows",
                    format!("行が多すぎます（最大 {}）", limits::MAX_TABLE_ROWS),
                )
                .at(path),
            );
        }
        for (i, col) in p.columns.iter().enumerate() {
            self.label(&col.label, &format!("{path}.columns[{i}].label"));
        }
        for (i, row) in p.rows.iter().enumerate() {
            if row.len() != p.columns.len() {
                self.errors.push(
                    GuiValidationError::new(
                        "gui.table_row_mismatch",
                        format!(
                            "行の長さ（{}）が列数（{}）と一致しません",
                            row.len(),
                            p.columns.len()
                        ),
                    )
                    .at(format!("{path}.rows[{i}]")),
                );
            }
            for (j, cell) in row.iter().enumerate() {
                if let crate::spec::CellValue::Text(t) = cell {
                    self.text(t, limits::MAX_TEXT_CHARS, &format!("{path}.rows[{i}][{j}]"));
                }
            }
        }
    }

    /// アクション参照が宣言済み束縛を指すこと（Task 6.3 受け入れ条件）。
    fn action_ref(&mut self, r: &ActionRef, path: &str) {
        if !self.action_ids.contains(&r.action.as_str()) {
            self.errors.push(
                GuiValidationError::new(
                    "gui.unknown_action_ref",
                    format!("アクション '{}' は actions に宣言されていません", r.action),
                )
                .at(path),
            );
        }
    }

    /// 本文テキストの上限＋制御文字（\n \t 以外）を拒否。補正（除去）はしない。
    fn text(&mut self, s: &str, max: usize, path: &str) {
        if s.chars().count() > max {
            self.errors.push(
                GuiValidationError::new(
                    "gui.string_too_long",
                    format!("文字列が長すぎます（最大 {max} 文字）"),
                )
                .at(path),
            );
        }
        if s.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
            self.errors.push(
                GuiValidationError::new("gui.control_char", "制御文字は使用できません").at(path),
            );
        }
    }

    /// ラベル類（1 行・短い）の検証。
    fn label(&mut self, s: &str, path: &str) {
        self.text(s, limits::MAX_LABEL_CHARS, path);
    }

    fn opt_label(&mut self, s: Option<&str>, path: &str) {
        if let Some(s) = s {
            self.label(s, path);
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
