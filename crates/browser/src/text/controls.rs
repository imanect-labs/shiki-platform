//! フォームの部品（入力欄・選択・ボタン）の描画と、送信に載る項目の収集（render.rs から分割）。
//!
//! 送信の規則は HTML 標準の form submission に合わせる:
//! - `disabled`（祖先の `<fieldset disabled>` を含む）は送らず、押せない。
//! - CSS で隠れていても送る（見えないので ref は振らない）。
//! - 送信ボタンは `formaction` / `formmethod` / `formenctype` でフォームの値を上書きする。
//! - `form="id"` で外のフォームに属する部品も、そのフォームの部品として扱う。

use dom_query::NodeRef;

use super::{button_token, collapse, label_text, Field, FieldKind, SubmitOverrides, Walker};
use crate::snapshot::{ElementKind, RefId};

impl Walker<'_> {
    /// 次に積む項目の利用者が入れた値。状態は ref ではなく**項目の位置**で持つ
    /// （ref の無い項目＝CSS で隠れたラジオ等も、同じグループの選択解除で書き換えるため）。
    fn pending_state(&self) -> Option<&super::FieldState> {
        self.state.get(&self.fields.len())
    }

    fn value(&self, default: String) -> String {
        self.pending_state()
            .and_then(|s| s.value.clone())
            .unwrap_or(default)
    }

    fn checked(&self, default: bool) -> bool {
        self.pending_state()
            .and_then(|s| s.checked)
            .unwrap_or(default)
    }

    fn is_disabled(&self, node: &NodeRef<'_>) -> bool {
        node.has_attr("disabled") || self.disabled > 0
    }

    /// 無効な部品のスナップショット上の注記。
    fn disabled_suffix(&self, node: &NodeRef<'_>) -> &'static str {
        if self.is_disabled(node) {
            " (無効)"
        } else {
            ""
        }
    }

    /// 部品がフォームに属するか（`form` 属性があればそれが優先し、無い id なら属さない）。
    fn has_owner(&self, node: &NodeRef<'_>, form: Option<usize>) -> bool {
        match node.attr("form") {
            Some(id) => self.form_ids.contains(&*id),
            None => form.is_some(),
        }
    }

    pub(super) fn push_field(
        &mut self,
        node: &NodeRef<'_>,
        form: Option<usize>,
        kind: FieldKind,
        element: Option<RefId>,
    ) {
        let disabled = self.is_disabled(node);
        self.fields.push(Field {
            form,
            name: node.attr("name").map(|n| n.to_string()).unwrap_or_default(),
            kind,
            element: element.map(|e| e.index),
            disabled,
            form_attr: node.attr("form").map(|f| f.to_string()),
        });
    }

    pub(super) fn textarea(&mut self, node: &NodeRef<'_>, form: Option<usize>, wrap: Option<&str>) {
        let label = self.label_for(node, wrap);
        let id = self.push_element(
            ElementKind::TextBox {
                input_type: "textarea".into(),
            },
            &label,
        );
        let value = self.value(node.text().to_string());
        self.push_field(
            node,
            form,
            FieldKind::Text {
                value: value.clone(),
                secret: false,
            },
            id,
        );
        if let Some(id) = id {
            let suffix = self.disabled_suffix(node);
            self.token(&format!(
                "[{id} textbox \"{label}\" value=\"{}\"{suffix}]",
                collapse(&value)
            ));
        }
    }

    /// 送信ボタンの `formaction` / `formmethod` / `formenctype`。
    fn overrides(&self, node: &NodeRef<'_>) -> Option<Box<SubmitOverrides>> {
        let o = SubmitOverrides {
            action: node
                .attr("formaction")
                .filter(|a| !a.trim().is_empty())
                .and_then(|a| self.resolve(&a)),
            post: node
                .attr("formmethod")
                .map(|m| m.eq_ignore_ascii_case("post")),
            enctype: node.attr("formenctype").map(|e| e.to_ascii_lowercase()),
        };
        (o != SubmitOverrides::default()).then(|| Box::new(o))
    }

    fn push_button(
        &mut self,
        node: &NodeRef<'_>,
        form: Option<usize>,
        label: &str,
        submit_type: bool,
    ) {
        let submits = submit_type && self.has_owner(node, form);
        let value = node
            .attr("value")
            .map(|v| v.to_string())
            .unwrap_or_default();
        let overrides = if submits { self.overrides(node) } else { None };
        let id = self.push_element(ElementKind::Button { submits }, label);
        self.push_field(
            node,
            form,
            FieldKind::Button {
                value,
                submits,
                overrides,
            },
            id,
        );
        if let Some(id) = id {
            let disabled = self.is_disabled(node);
            self.token(&button_token(id, label, submits, disabled));
        }
    }

    pub(super) fn button(&mut self, node: &NodeRef<'_>, form: Option<usize>) {
        let submit_type = node
            .attr("type")
            .is_none_or(|t| t.eq_ignore_ascii_case("submit"));
        let mut label = label_text(node);
        if label.is_empty() {
            label = self.label_for(node, None);
        }
        self.push_button(node, form, &label, submit_type);
    }

    pub(super) fn input(&mut self, node: &NodeRef<'_>, form: Option<usize>, wrap: Option<&str>) {
        let ty = node
            .attr("type")
            .map_or_else(|| "text".to_string(), |t| t.to_ascii_lowercase());
        let value = node
            .attr("value")
            .map(|v| v.to_string())
            .unwrap_or_default();
        let label = self.label_for(node, wrap);
        match ty.as_str() {
            "checkbox" | "radio" => {
                let kind = if ty == "radio" {
                    ElementKind::Radio
                } else {
                    ElementKind::Checkbox
                };
                let id = self.push_element(kind, &label);
                let checked = self.checked(node.has_attr("checked"));
                let value = if value.is_empty() { "on".into() } else { value };
                self.push_field(
                    node,
                    form,
                    FieldKind::Check {
                        value,
                        checked,
                        radio: ty == "radio",
                    },
                    id,
                );
                if let Some(id) = id {
                    let suffix = self.disabled_suffix(node);
                    self.token(&format!(
                        "[{id} {ty} \"{label}\" checked={checked}{suffix}]"
                    ));
                }
            }
            "submit" | "image" | "button" | "reset" => {
                let text = if value.is_empty() {
                    "送信".to_string()
                } else {
                    value
                };
                self.push_button(node, form, &text, matches!(ty.as_str(), "submit" | "image"));
            }
            "file" => self.text(&format!(" （ファイル選択欄「{label}」は扱えません） ")),
            _ => {
                let id = self.push_element(
                    ElementKind::TextBox {
                        input_type: ty.clone(),
                    },
                    &label,
                );
                let value = self.value(value);
                self.push_field(
                    node,
                    form,
                    FieldKind::Text {
                        value: value.clone(),
                        secret: ty == "password",
                    },
                    id,
                );
                if let Some(id) = id {
                    // パスワード欄の値は常に伏せる（browser.md §4）。
                    let shown = if ty == "password" {
                        "*".repeat(value.chars().count())
                    } else {
                        value
                    };
                    let suffix = self.disabled_suffix(node);
                    self.token(&format!(
                        "[{id} textbox \"{label}\" type={ty} value=\"{shown}\"{suffix}]"
                    ));
                }
            }
        }
    }

    pub(super) fn select(&mut self, node: &NodeRef<'_>, form: Option<usize>, wrap: Option<&str>) {
        let label = self.label_for(node, wrap);
        let multiple = node.has_attr("multiple");
        // (value, 表示名, 無効か)。`<option disabled>` と `<optgroup disabled>` の中の選択肢は選べず、送らない。
        let mut all: Vec<(String, String, bool)> = Vec::new();
        let mut selected = Vec::new();
        for d in node.descendants_it() {
            if d.node_name().is_some_and(|n| &*n == "option") {
                let text = collapse(&d.text());
                let value = d
                    .attr("value")
                    .map_or_else(|| text.clone(), |v| v.to_string());
                let disabled = d.has_attr("disabled")
                    || d.parent().is_some_and(|p| {
                        p.node_name().is_some_and(|n| &*n == "optgroup") && p.has_attr("disabled")
                    });
                if d.has_attr("selected") && !disabled {
                    if !multiple {
                        selected.clear();
                    }
                    selected.push(value.clone());
                }
                all.push((value, text, disabled));
            }
        }
        // 単一選択で何も選ばれていなければ、最初の有効な選択肢が選ばれる。複数選択・選択肢なしは何も選ばない。
        if !multiple && selected.is_empty() {
            if let Some((v, _, _)) = all.iter().find(|(_, _, d)| !d) {
                selected.push(v.clone());
            }
        }
        let listed = all
            .iter()
            .map(|(v, t, d)| {
                if *d {
                    format!("{v:?}={t}(無効)")
                } else {
                    format!("{v:?}={t}")
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let options: Vec<(String, String)> = all
            .into_iter()
            .filter(|(_, _, d)| !d)
            .map(|(v, t, _)| (v, t))
            .collect();
        let id = self.push_element(ElementKind::Select { options, multiple }, &label);
        let values = self
            .pending_state()
            .and_then(|s| s.selected.clone())
            .unwrap_or(selected);
        self.push_field(
            node,
            form,
            FieldKind::Select {
                values: values.clone(),
            },
            id,
        );
        if let Some(id) = id {
            let shown = if multiple {
                format!("{values:?}")
            } else {
                format!("{:?}", values.first().map_or("", String::as_str))
            };
            let kind = if multiple {
                "select multiple"
            } else {
                "select"
            };
            let suffix = self.disabled_suffix(node);
            self.token(&format!(
                "[{id} {kind} \"{label}\" value={shown} options: {listed}{suffix}]"
            ));
        }
    }
}
