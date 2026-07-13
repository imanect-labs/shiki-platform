//! コンポーネントツリーの走査検証（[`validate_spec`](super::validate_spec) の下請け）。
//!
//! 深さ/個数/文字列上限・アクション参照存在・予約 variant 拒否をツリー全体で全件収集する。

use super::{limits, GuiValidationError};
use crate::spec::UiNode;

mod leaf;

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
            UiNode::QuestionCard(p) => self.question_card(p, path),
            // available() 判定で早期 return 済み。
            UiNode::Map(_) | UiNode::Image(_) => unreachable!("reserved components return early"),
        }
    }
}
