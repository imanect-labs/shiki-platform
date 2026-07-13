//! ノード検証の下請けメソッド群（[`Walk`](super::Walk) の一部・ファイル分割）。

use super::Walk;
use crate::spec::{ActionRef, FormField, UiNode};
use crate::validate::{check_id, limits, GuiValidationError};

impl Walk<'_> {
    pub(super) fn form(&mut self, p: &crate::spec::FormProps, path: &str) {
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
                    self.choice_options(&f.options, &fpath);
                    // allow_other のときは選択肢外（自由記述）の default を許す。
                    if let Some(d) = &f.default {
                        if !f.allow_other && !f.options.iter().any(|o| &o.value == d) {
                            self.invalid_default(&fpath);
                        }
                    }
                }
                FormField::Checkbox(f) => {
                    self.label(&f.label, &format!("{fpath}.label"));
                    self.choice_options(&f.options, &fpath);
                    for (j, d) in f.default.iter().enumerate() {
                        if !f.allow_other && !f.options.iter().any(|o| &o.value == d) {
                            self.invalid_default(&format!("{fpath}.default[{j}]"));
                        }
                    }
                }
                FormField::Radio(f) => {
                    self.label(&f.label, &format!("{fpath}.label"));
                    self.choice_options(&f.options, &fpath);
                    if let Some(d) = &f.default {
                        if !f.allow_other && !f.options.iter().any(|o| &o.value == d) {
                            self.invalid_default(&fpath);
                        }
                    }
                }
                FormField::Date(f) => {
                    self.label(&f.label, &format!("{fpath}.label"));
                    self.opt_label(f.min.as_deref(), &format!("{fpath}.min"));
                    self.opt_label(f.max.as_deref(), &format!("{fpath}.max"));
                    self.opt_label(f.default.as_deref(), &format!("{fpath}.default"));
                }
                FormField::Slider(f) => {
                    self.label(&f.label, &format!("{fpath}.label"));
                    if !(f.min.is_finite() && f.max.is_finite()) || f.min >= f.max {
                        self.errors.push(
                            GuiValidationError::new(
                                "gui.invalid_range",
                                "slider は min < max（ともに有限数）が必要です",
                            )
                            .at(&fpath),
                        );
                    }
                    if f.step.is_some_and(|s| !(s.is_finite() && s > 0.0)) {
                        self.errors.push(
                            GuiValidationError::new("gui.invalid_number", "step は正の有限数のみ")
                                .at(format!("{fpath}.step")),
                        );
                    }
                }
                FormField::Rating(f) => {
                    self.label(&f.label, &format!("{fpath}.label"));
                    let max = f.max.unwrap_or(5);
                    if !(1..=limits::MAX_RATING).contains(&max) {
                        self.errors.push(
                            GuiValidationError::new(
                                "gui.invalid_range",
                                format!("rating の max は 1〜{}", limits::MAX_RATING),
                            )
                            .at(format!("{fpath}.max")),
                        );
                    }
                    if f.default.is_some_and(|d| d > max) {
                        self.invalid_default(&fpath);
                    }
                }
            }
        }
    }

    /// 選択肢（select/radio/checkbox 共通）の個数・ラベル検証。
    pub(super) fn choice_options(&mut self, options: &[crate::spec::SelectOption], fpath: &str) {
        if options.len() > limits::MAX_SELECT_OPTIONS {
            self.errors.push(
                GuiValidationError::new(
                    "gui.too_many_options",
                    format!("選択肢が多すぎます（最大 {}）", limits::MAX_SELECT_OPTIONS),
                )
                .at(fpath),
            );
        }
        for (j, opt) in options.iter().enumerate() {
            self.label(&opt.value, &format!("{fpath}.options[{j}].value"));
            self.label(&opt.label, &format!("{fpath}.options[{j}].label"));
        }
    }

    pub(super) fn invalid_default(&mut self, path: &str) {
        self.errors.push(
            GuiValidationError::new(
                "gui.invalid_default",
                "default は options / 範囲から選んでください",
            )
            .at(format!("{path}.default")),
        );
    }

    pub(super) fn table(&mut self, p: &crate::spec::TableProps, path: &str) {
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

    pub(super) fn stat(&mut self, p: &crate::spec::StatProps, path: &str) {
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
    pub(super) fn count(&mut self, len: usize, path: &str) {
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
    pub(super) fn children(&mut self, children: &[UiNode], path: &str, depth: usize) {
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
    pub(super) fn action_ref(&mut self, r: &ActionRef, path: &str) {
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
    pub(super) fn text(&mut self, s: &str, max: usize, path: &str) {
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
    pub(super) fn label(&mut self, s: &str, path: &str) {
        self.text(s, limits::MAX_LABEL_CHARS, path);
    }

    pub(super) fn opt_label(&mut self, s: Option<&str>, path: &str) {
        if let Some(s) = s {
            self.label(s, path);
        }
    }
}
