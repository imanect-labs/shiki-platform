//! コンポーネントツリーの走査検証（[`validate_spec`](super::validate_spec) の下請け）。
//!
//! 深さ/個数/文字列上限・アクション参照存在・予約 variant 拒否をツリー全体で全件収集する。

use crate::spec::{ActionRef, FormField, UiNode};

use super::{check_id, limits, GuiValidationError};

/// ツリー走査の状態。
pub(super) struct Walk<'a> {
    errors: &'a mut Vec<GuiValidationError>,
    action_ids: &'a [&'a str],
    node_count: usize,
    node_overflow: bool,
    form_ids: Vec<String>,
}

impl<'a> Walk<'a> {
    pub(super) fn new(errors: &'a mut Vec<GuiValidationError>, action_ids: &'a [&'a str]) -> Self {
        Walk {
            errors,
            action_ids,
            node_count: 0,
            node_overflow: false,
            form_ids: Vec::new(),
        }
    }

    #[allow(clippy::too_many_lines)] // コンポーネント別の検証分岐（分割すると対応が読みにくい）。
    pub(super) fn node(&mut self, node: &UiNode, path: &str, depth: usize) {
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
                // 面積/割合で大小を表す種別は負値が無意味（描画で黙って 0 化すると欠落して誤解を招く）。
                use crate::vocab::ChartKind;
                let magnitude_only = matches!(
                    spec.kind,
                    ChartKind::Pie
                        | ChartKind::Donut
                        | ChartKind::Funnel
                        | ChartKind::Treemap
                        | ChartKind::RadialBar
                );
                for (i, point) in spec.data.iter().enumerate() {
                    self.label(&point.x, &format!("{path}.data[{i}].x"));
                    self.opt_label(point.series.as_deref(), &format!("{path}.data[{i}].series"));
                    if !point.y.is_finite() {
                        self.errors.push(
                            GuiValidationError::new("gui.invalid_number", "y は有限数のみ")
                                .at(format!("{path}.data[{i}].y")),
                        );
                    }
                    if magnitude_only && point.y < 0.0 {
                        self.errors.push(
                            GuiValidationError::new(
                                "gui.negative_not_allowed",
                                "この種別（pie/donut/funnel/treemap/radial_bar）では y に負値を使えません",
                            )
                            .at(format!("{path}.data[{i}].y")),
                        );
                    }
                    if point.xv.is_some_and(|v| !v.is_finite()) {
                        self.errors.push(
                            GuiValidationError::new("gui.invalid_number", "xv は有限数のみ")
                                .at(format!("{path}.data[{i}].xv")),
                        );
                    }
                }
                if spec.line_series.len() > limits::MAX_LINE_SERIES {
                    self.errors.push(
                        GuiValidationError::new(
                            "gui.too_many_line_series",
                            format!(
                                "line_series が多すぎます（最大 {}）",
                                limits::MAX_LINE_SERIES
                            ),
                        )
                        .at(path),
                    );
                }
                for (i, s) in spec.line_series.iter().enumerate() {
                    self.label(s, &format!("{path}.line_series[{i}]"));
                }
            }
            UiNode::Stat(p) => self.stat(p, path),
            UiNode::Callout(p) => {
                self.opt_label(p.title.as_deref(), &format!("{path}.title"));
                self.text(&p.text, limits::MAX_TEXT_CHARS, &format!("{path}.text"));
            }
            UiNode::Accordion(p) => {
                self.count(p.items.len(), &format!("{path}.items"));
                for (i, item) in p.items.iter().enumerate() {
                    self.label(&item.title, &format!("{path}.items[{i}].title"));
                    self.children(&item.children, &format!("{path}.items[{i}]"), depth);
                }
            }
            UiNode::Tabs(p) => {
                self.count(p.tabs.len(), &format!("{path}.tabs"));
                for (i, tab) in p.tabs.iter().enumerate() {
                    self.label(&tab.label, &format!("{path}.tabs[{i}].label"));
                    self.children(&tab.children, &format!("{path}.tabs[{i}]"), depth);
                }
            }
            UiNode::Stepper(p) => {
                self.count(p.steps.len(), &format!("{path}.steps"));
                for (i, s) in p.steps.iter().enumerate() {
                    self.label(&s.title, &format!("{path}.steps[{i}].title"));
                    if let Some(d) = &s.description {
                        self.text(
                            d,
                            limits::MAX_TEXT_CHARS,
                            &format!("{path}.steps[{i}].description"),
                        );
                    }
                }
            }
            UiNode::BadgeList(p) => {
                self.count(p.badges.len(), &format!("{path}.badges"));
                for (i, b) in p.badges.iter().enumerate() {
                    self.label(&b.label, &format!("{path}.badges[{i}].label"));
                }
            }
            UiNode::KeyValue(p) => {
                self.opt_label(p.title.as_deref(), &format!("{path}.title"));
                self.count(p.items.len(), &format!("{path}.items"));
                for (i, kv) in p.items.iter().enumerate() {
                    self.label(&kv.key, &format!("{path}.items[{i}].key"));
                    self.text(
                        &kv.value,
                        limits::MAX_TEXT_CHARS,
                        &format!("{path}.items[{i}].value"),
                    );
                }
            }
            UiNode::CodeBlock(p) => {
                self.text(&p.code, limits::MAX_CODE_CHARS, &format!("{path}.code"));
                self.opt_label(p.language.as_deref(), &format!("{path}.language"));
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

    fn stat(&mut self, p: &crate::spec::StatProps, path: &str) {
        self.label(&p.label, &format!("{path}.label"));
        self.label(&p.value, &format!("{path}.value"));
        self.opt_label(p.unit.as_deref(), &format!("{path}.unit"));
        self.opt_label(p.delta_label.as_deref(), &format!("{path}.delta_label"));
        self.opt_label(p.caption.as_deref(), &format!("{path}.caption"));
        if p.delta.is_some_and(|v| !v.is_finite()) {
            self.errors.push(
                GuiValidationError::new("gui.invalid_number", "delta は有限数のみ")
                    .at(format!("{path}.delta")),
            );
        }
        if p.trend.len() > limits::MAX_SPARKLINE_POINTS {
            self.errors.push(
                GuiValidationError::new(
                    "gui.too_many_points",
                    format!(
                        "trend の点が多すぎます（最大 {}）",
                        limits::MAX_SPARKLINE_POINTS
                    ),
                )
                .at(path),
            );
        }
        for (i, v) in p.trend.iter().enumerate() {
            if !v.is_finite() {
                self.errors.push(
                    GuiValidationError::new("gui.invalid_number", "trend は有限数のみ")
                        .at(format!("{path}.trend[{i}]")),
                );
            }
        }
    }

    /// 項目数の上限（accordion/tabs/stepper/badge/key_value 等の直下要素数）。
    fn count(&mut self, len: usize, path: &str) {
        if len > limits::MAX_CHILDREN {
            self.errors.push(
                GuiValidationError::new(
                    "gui.too_many_children",
                    format!("要素が多すぎます（最大 {}）", limits::MAX_CHILDREN),
                )
                .at(path),
            );
        }
    }

    /// 子ツリー列を走査する（数の上限＋各子を depth+1 で再帰検証）。
    fn children(&mut self, children: &[UiNode], path: &str, depth: usize) {
        if children.len() > limits::MAX_CHILDREN {
            self.errors.push(
                GuiValidationError::new(
                    "gui.too_many_children",
                    format!("子要素が多すぎます（最大 {}）", limits::MAX_CHILDREN),
                )
                .at(path),
            );
        }
        for (i, child) in children.iter().enumerate() {
            self.node(child, &format!("{path}.children[{i}]"), depth + 1);
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
