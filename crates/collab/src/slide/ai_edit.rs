//! AI 共同編集の編集オペレーション（スライド版・Task 11.3・design §4.8.3）。
//!
//! エージェントの `slide.edit` は**共同編集参加者**として、人間と同じ Yjs ドキュメントに
//! 変更を適用する（別コピーを作らない・ノートの [`crate::note::ai_edit`] と同一原則）。
//! スライドは自由 HTML のため、**すべての HTML 入力を適用前に ammonia でサニタイズ**する
//! （PIT-40 第1層・サーバが最後の砦）。
//!
//! 操作はスライド単位＋メタ操作の粗い集合（LLM が安定して出せる粒度）:
//! - `AppendSlide` / `InsertSlideAfter`: 追加（非破壊・最も安全）
//! - `ReplaceSlide` / `RemoveSlide` / `SetNotes` / `SetBackground`: id 指定の変更
//! - `SetMeta`: タイトル・テーマ等の frontmatter 型属性

use serde::Deserialize;
use uuid::Uuid;
use yrs::{Any, Array, ArrayRef, Map, MapRef, Out, TransactionMut};

use super::sanitize::sanitize_html;
use crate::note::yjs_meta::write_meta_pair;

pub use crate::note::ai_edit::EditReport;

/// 1 つの編集操作。スライドの参照は安定 id（`.slide` JSON / Yjs の `id`）。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SlideEditOp {
    /// 末尾にスライドを追加する。
    AppendSlide {
        html: String,
        #[serde(default)]
        notes: Option<String>,
    },
    /// `slide_id` の直後にスライドを挿入する。
    InsertSlideAfter {
        slide_id: String,
        html: String,
        #[serde(default)]
        notes: Option<String>,
    },
    /// `slide_id` の本文 HTML を置換する。
    ReplaceSlide { slide_id: String, html: String },
    /// `slide_id` を削除する。
    RemoveSlide { slide_id: String },
    /// `slide_id` のスピーカーノートを置換する。
    SetNotes { slide_id: String, notes: String },
    /// `slide_id` の背景（`{"color": "#rrggbb"}` 等の JSON）を設定する。
    SetBackground {
        slide_id: String,
        bg: serde_json::Value,
    },
    /// メタデータ（title/theme_id/tags/任意 kv）を 1 件設定する。
    SetMeta { key: String, value: String },
}

/// 編集オペ列をトランザクションに適用する（部分適用・失敗は skip として記録）。
pub fn apply_ops(
    txn: &mut TransactionMut<'_>,
    slides: &ArrayRef,
    meta: &MapRef,
    ops: &[SlideEditOp],
) -> EditReport {
    let mut report = EditReport::default();
    for op in ops {
        if apply_one(txn, slides, meta, op) {
            report.applied += 1;
        } else {
            report.skipped.push(describe(op));
        }
    }
    report
}

fn apply_one(
    txn: &mut TransactionMut<'_>,
    slides: &ArrayRef,
    meta: &MapRef,
    op: &SlideEditOp,
) -> bool {
    match op {
        SlideEditOp::AppendSlide { html, notes } => {
            let slide = new_slide(html, notes.as_deref());
            slides.push_back(txn, slide);
            true
        }
        SlideEditOp::InsertSlideAfter {
            slide_id,
            html,
            notes,
        } => {
            let Some(index) = find_index(txn, slides, slide_id) else {
                return false;
            };
            slides.insert(txn, index + 1, new_slide(html, notes.as_deref()));
            true
        }
        SlideEditOp::ReplaceSlide { slide_id, html } => {
            with_slide(txn, slides, slide_id, |txn, slide| {
                replace_text(txn, &slide, "html", &sanitize_html(html));
            })
        }
        SlideEditOp::RemoveSlide { slide_id } => {
            let Some(index) = find_index(txn, slides, slide_id) else {
                return false;
            };
            slides.remove_range(txn, index, 1);
            true
        }
        SlideEditOp::SetNotes { slide_id, notes } => {
            with_slide(txn, slides, slide_id, |txn, slide| {
                replace_text(txn, &slide, "notes", notes);
            })
        }
        SlideEditOp::SetBackground { slide_id, bg } => {
            if !bg.is_object() {
                return false;
            }
            with_slide(txn, slides, slide_id, |txn, slide| {
                slide.insert(txn, "bg", Any::from(bg.to_string()));
            })
        }
        SlideEditOp::SetMeta { key, value } => {
            write_meta_pair(txn, meta, key, value);
            true
        }
    }
}

/// 新しいスライドの Yjs 挿入表現（HTML はサニタイズ・id は採番）。
fn new_slide(html: &str, notes: Option<&str>) -> yrs::MapPrelim {
    super::yjs_doc::slide_prelim(&super::model::Slide {
        id: Uuid::new_v4().to_string(),
        html: sanitize_html(html),
        notes: notes.unwrap_or_default().to_string(),
        bg: None,
    })
}

fn find_index(txn: &TransactionMut<'_>, slides: &ArrayRef, slide_id: &str) -> Option<u32> {
    slides
        .iter(txn)
        .position(|entry| match entry {
            Out::YMap(map) => {
                matches!(map.get(txn, "id"), Some(Out::Any(Any::String(s))) if &*s == slide_id)
            }
            _ => false,
        })
        .map(|i| i as u32)
}

fn with_slide(
    txn: &mut TransactionMut<'_>,
    slides: &ArrayRef,
    slide_id: &str,
    apply: impl FnOnce(&mut TransactionMut<'_>, MapRef),
) -> bool {
    let Some(index) = find_index(txn, slides, slide_id) else {
        return false;
    };
    let Some(Out::YMap(slide)) = slides.get(txn, index) else {
        return false;
    };
    apply(txn, slide);
    true
}

/// Y.Text 値を全置換する（無ければ新設。AI 編集はスライド単位の置換が契約のため全置換でよい）。
fn replace_text(txn: &mut TransactionMut<'_>, slide: &MapRef, key: &str, value: &str) {
    use yrs::Text;
    if let Some(Out::YText(text)) = slide.get(txn, key) {
        let len = text.len(txn);
        if len > 0 {
            text.remove_range(txn, 0, len);
        }
        text.insert(txn, 0, value);
    } else {
        let prelim: yrs::In = yrs::In::Text(yrs::TextPrelim::new(value.to_string()).into());
        slide.insert(txn, key, prelim);
    }
}

fn describe(op: &SlideEditOp) -> String {
    match op {
        SlideEditOp::AppendSlide { .. } => "append_slide".to_string(),
        SlideEditOp::InsertSlideAfter { slide_id, .. } => {
            format!("insert_slide_after: {slide_id} が見つかりません")
        }
        SlideEditOp::ReplaceSlide { slide_id, .. } => {
            format!("replace_slide: {slide_id} が見つかりません")
        }
        SlideEditOp::RemoveSlide { slide_id } => {
            format!("remove_slide: {slide_id} が見つかりません")
        }
        SlideEditOp::SetNotes { slide_id, .. } => {
            format!("set_notes: {slide_id} が見つかりません")
        }
        SlideEditOp::SetBackground { slide_id, .. } => {
            format!("set_background: {slide_id} が見つからないか bg がオブジェクトではありません")
        }
        SlideEditOp::SetMeta { key, .. } => format!("set_meta: {key}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slide::yjs_doc::{read_slides, write_slides, SLIDES_ARRAY_NAME};
    use crate::slide::Slide;
    use yrs::{Doc, Transact};

    fn setup(doc: &Doc) -> (ArrayRef, MapRef) {
        let slides = doc.get_or_insert_array(SLIDES_ARRAY_NAME);
        let meta = doc.get_or_insert_map(crate::note::yjs_map::META_MAP_NAME);
        {
            let mut txn = doc.transact_mut();
            write_slides(
                &mut txn,
                &slides,
                &[Slide {
                    id: "s1".into(),
                    html: "<h1>表紙</h1>".into(),
                    notes: String::new(),
                    bg: None,
                }],
            );
        }
        (slides, meta)
    }

    #[test]
    fn 追加と挿入と削除が適用される() {
        let doc = Doc::new();
        let (slides, meta) = setup(&doc);
        let mut txn = doc.transact_mut();
        let report = apply_ops(
            &mut txn,
            &slides,
            &meta,
            &[
                SlideEditOp::AppendSlide {
                    html: "<h2>まとめ</h2>".into(),
                    notes: Some("締め".into()),
                },
                SlideEditOp::InsertSlideAfter {
                    slide_id: "s1".into(),
                    html: "<h2>アジェンダ</h2>".into(),
                    notes: None,
                },
            ],
        );
        assert_eq!(report.applied, 2);
        drop(txn);
        let txn = doc.transact();
        let read = read_slides(&txn, &slides);
        assert_eq!(read.len(), 3);
        assert!(read[1].html.contains("アジェンダ"));
        assert!(read[2].html.contains("まとめ"));
        assert_eq!(read[2].notes, "締め");
    }

    #[test]
    fn 敵対的htmlは適用時に落ちる() {
        let doc = Doc::new();
        let (slides, meta) = setup(&doc);
        let mut txn = doc.transact_mut();
        apply_ops(
            &mut txn,
            &slides,
            &meta,
            &[
                SlideEditOp::ReplaceSlide {
                    slide_id: "s1".into(),
                    html: r#"<script>alert(1)</script><p onclick="x()">安全な本文</p>"#.into(),
                },
                SlideEditOp::AppendSlide {
                    html: r#"<iframe src="https://evil"></iframe><h2>追加</h2>"#.into(),
                    notes: None,
                },
            ],
        );
        drop(txn);
        let txn = doc.transact();
        let read = read_slides(&txn, &slides);
        assert!(!read[0].html.contains("script"));
        assert!(!read[0].html.contains("onclick"));
        assert!(read[0].html.contains("安全な本文"));
        assert!(!read[1].html.contains("iframe"));
    }

    #[test]
    fn 見つからないidはskipされ部分適用になる() {
        let doc = Doc::new();
        let (slides, meta) = setup(&doc);
        let mut txn = doc.transact_mut();
        let report = apply_ops(
            &mut txn,
            &slides,
            &meta,
            &[
                SlideEditOp::RemoveSlide {
                    slide_id: "missing".into(),
                },
                SlideEditOp::SetMeta {
                    key: "title".into(),
                    value: "提案書".into(),
                },
            ],
        );
        assert_eq!(report.applied, 1);
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0].contains("remove_slide"));
    }

    #[test]
    fn 背景は非オブジェクトを拒否する() {
        let doc = Doc::new();
        let (slides, meta) = setup(&doc);
        let mut txn = doc.transact_mut();
        let report = apply_ops(
            &mut txn,
            &slides,
            &meta,
            &[SlideEditOp::SetBackground {
                slide_id: "s1".into(),
                bg: serde_json::json!("javascript:alert(1)"),
            }],
        );
        assert_eq!(report.applied, 0);
        assert_eq!(report.skipped.len(), 1);
    }
}
