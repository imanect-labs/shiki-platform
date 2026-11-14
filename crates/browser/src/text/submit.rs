//! フォームの送信要求を組み立てる（HTML 標準の form submission に合わせる・mod.rs から分割）。
//!
//! - 項目は文書順。`disabled` と名前の無い項目は送らない。
//! - チェックボックス・ラジオは選ばれているものだけ、セレクトは選ばれている value の数だけ載る。
//! - 押したボタン（submitter）の name=value だけが載る。submitter の `formaction` / `formmethod` /
//!   `formenctype` はフォームの値を上書きする。
//! - Enter による送信（[`default_submitter`]）は、フォームの最初の送信ボタンを submitter にする。
//!   それが無効なら送信しない。

use url::Url;

use super::render::{Field, FieldKind, Form};
use crate::engine::{BrowserError, LoadRequest, Method};

/// Enter で送信したときの submitter（フォームの最初の送信ボタン）。
///
/// # Errors
/// 最初の送信ボタンが無効なら送信しない（ブラウザも送らない）。
pub(crate) fn default_submitter(
    fields: &[Field],
    form_index: usize,
) -> Result<Option<usize>, BrowserError> {
    match fields.iter().position(|f| {
        f.form == Some(form_index) && matches!(f.kind, FieldKind::Button { submits: true, .. })
    }) {
        Some(i) if fields[i].disabled => Err(BrowserError::NotInteractive(
            "送信（既定の送信ボタンが無効）",
        )),
        other => Ok(other),
    }
}

/// フォームを送信する要求。`submitter` は押したボタンの項目の位置（`rendered.fields` の添字）。
///
/// # Errors
/// フォームが無い・送信方式（multipart 等）に対応していない。
pub(crate) fn build(
    forms: &[Form],
    fields: &[Field],
    page_url: &Url,
    form_index: usize,
    submitter: Option<usize>,
) -> Result<LoadRequest, BrowserError> {
    let form = forms
        .get(form_index)
        .ok_or(BrowserError::NotInteractive("送信"))?;
    let overrides = submitter
        .and_then(|i| fields.get(i))
        .and_then(|f| match &f.kind {
            FieldKind::Button { overrides, .. } => overrides.as_deref(),
            _ => None,
        });
    if overrides.is_some_and(|o| o.bad_action) {
        return Err(BrowserError::Unsupported(
            "formaction（http/https 以外）".into(),
        ));
    }
    let action = match overrides.and_then(|o| o.action.clone()) {
        Some(a) => a,
        // 明示された action を解決できない（`javascript:` 等）フォームは送らない。
        None => form
            .action
            .clone()
            .ok_or_else(|| BrowserError::Unsupported("action（http/https 以外）".into()))?,
    };
    if overrides.and_then(|o| o.dialog).unwrap_or(form.dialog) {
        return Err(BrowserError::Unsupported("method=dialog".into()));
    }
    let post = overrides.and_then(|o| o.post).unwrap_or(form.post);
    let enctype = overrides
        .and_then(|o| o.enctype.clone())
        .unwrap_or_else(|| form.enctype.clone());
    if post && (enctype.starts_with("multipart/") || enctype.starts_with("text/plain")) {
        return Err(BrowserError::Unsupported(enctype));
    }

    let mut pairs = Vec::new();
    for (i, f) in fields.iter().enumerate() {
        if f.form != Some(form_index) || f.disabled {
            continue;
        }
        // 押した画像ボタンは座標を送る（名前が無ければ x / y）。座標はテキストエンジンには無いので 0。
        if let FieldKind::Button { image: true, .. } = &f.kind {
            if Some(i) == submitter {
                let (x, y) = if f.name.is_empty() {
                    ("x".to_string(), "y".to_string())
                } else {
                    (format!("{}.x", f.name), format!("{}.y", f.name))
                };
                pairs.push((x, "0".to_string()));
                pairs.push((y, "0".to_string()));
            }
            continue;
        }
        if f.name.is_empty() {
            continue;
        }
        match &f.kind {
            FieldKind::Text { value, .. } | FieldKind::Hidden { value } => {
                pairs.push((f.name.clone(), value.clone()));
            }
            FieldKind::Select { values } => {
                pairs.extend(values.iter().map(|v| (f.name.clone(), v.clone())));
            }
            FieldKind::Check {
                value,
                checked: true,
                ..
            } => pairs.push((f.name.clone(), value.clone())),
            FieldKind::Button { value, .. } if Some(i) == submitter => {
                pairs.push((f.name.clone(), value.clone()));
            }
            FieldKind::Check { .. } | FieldKind::Button { .. } => {}
        }
    }
    let initiator = Some(page_url.clone());
    if post {
        Ok(LoadRequest {
            approved_origin: Some(action.origin()),
            url: action,
            method: Method::Post,
            form: pairs,
            initiator,
        })
    } else {
        let mut url = action;
        url.set_fragment(None);
        url.query_pairs_mut().clear().extend_pairs(&pairs);
        Ok(LoadRequest {
            url,
            method: Method::Get,
            form: Vec::new(),
            initiator,
            approved_origin: None,
        })
    }
}
