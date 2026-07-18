//! スライドドキュメント種（Task 11.1・design §4.8.3）。
//!
//! **真実は Yjs ドキュメント**。正規化 JSON（[`model`]）は保存時のシリアライズ形式であり、
//! `JSON ⇔ SlideDoc ⇔ Yjs` で往復する。すべての書込経路（保存・インポート・AI 編集・
//! 下書き確定）は [`sanitize`] を通し、「サニタイズ済みが正規形」を保証する（PIT-40 第1層）。
//!
//! # 往復保証の契約
//!
//! 次は「Yjs → JSON → Yjs → JSON」で正規形が安定する: スライドの id/並び・
//! サニタイズ済み html・notes・bg（JSON object）・メタ（title/icon/tags/thread_id/任意 kv）。
//! **JSON に落ちない情報**（awareness・CRDT 内部参照・サジェストマーク等）は往復対象外で、
//! Yjs snapshot（collab_doc.snapshot）を正本として保全する（ノートと同じ規約）。
//!
//! # 外部書込の取り込み（単方向規約）
//!
//! `.slide` ファイル側の直接書込はロード時に JSON をパース→サニタイズ→Yjs へ全置換で
//! 取り込む。**パース不能な JSON はエラー**（fail-closed）— 壊れたファイルを空ドキュメント
//! として開いて上書き保存し、データを失う事故を防ぐ。

pub mod ai_edit;
pub mod model;
pub mod sanitize;
pub mod yjs_doc;

pub use ai_edit::SlideEditOp;
pub use model::{Slide, SlideDoc, SLIDE_DOC_VERSION};
pub use sanitize::sanitize_html;

use yrs::{Doc, Transact};

use crate::error::CollabError;
use crate::note::{yjs_map, yjs_meta};

/// スライド（`.slide`）としてこの collab 経路の対象になるファイルか。
pub fn is_slide_file(name: &str) -> bool {
    crate::doc_kind::DocKind::from_name(name) == Some(crate::doc_kind::DocKind::Slide)
}

/// Yjs ドキュメント全体を正規化 JSON へシリアライズする（html はサニタイズ済みが正規形）。
pub fn doc_to_slide_json(doc: &Doc) -> String {
    let array = doc.get_or_insert_array(yjs_doc::SLIDES_ARRAY_NAME);
    let meta_map = doc.get_or_insert_map(yjs_map::META_MAP_NAME);
    let txn = doc.transact();
    let mut slides = yjs_doc::read_slides(&txn, &array);
    for slide in &mut slides {
        slide.html = sanitize_html(&slide.html);
    }
    let meta = yjs_meta::read_meta(&txn, &meta_map);
    SlideDoc { meta, slides }.to_json()
}

/// 正規化 JSON を Yjs ドキュメントへ**全置換**で取り込む（インポート・fail-closed）。
pub fn import_slide_json(doc: &Doc, json: &str) -> Result<(), CollabError> {
    let parsed = parse_and_sanitize(json)?;
    let array = doc.get_or_insert_array(yjs_doc::SLIDES_ARRAY_NAME);
    let meta_map = doc.get_or_insert_map(yjs_map::META_MAP_NAME);
    let mut txn = doc.transact_mut();
    yjs_meta::write_meta(&mut txn, &meta_map, &parsed.meta);
    yjs_doc::write_slides(&mut txn, &array, &parsed.slides);
    Ok(())
}

/// JSON 文字列を正規形（パース→サニタイズ→再シリアライズ）へ正規化する
/// （作成 API・下書き確定経路で使用。生 HTML の流入をここで遮断する）。
pub fn normalize_slide_json(json: &str) -> Result<String, CollabError> {
    Ok(parse_and_sanitize(json)?.to_json())
}

fn parse_and_sanitize(json: &str) -> Result<SlideDoc, CollabError> {
    let mut parsed = SlideDoc::from_json(json)
        .map_err(|e| CollabError::InvalidUpdate(format!("不正なスライド JSON: {e}")))?;
    for slide in &mut parsed.slides {
        slide.html = sanitize_html(&slide.html);
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_json() -> String {
        SlideDoc {
            meta: crate::note::NoteMeta {
                title: Some("提案".into()),
                ..Default::default()
            },
            slides: vec![Slide {
                id: "s1".into(),
                html: "<h1>表紙</h1><p>こんにちは</p>".into(),
                notes: "つかみ".into(),
                bg: Some(serde_json::json!({"color": "#0a0a0a"})),
            }],
        }
        .to_json()
    }

    #[test]
    fn json往復で正規形が安定する() {
        let doc = Doc::new();
        import_slide_json(&doc, &sample_json()).expect("import");
        let out = doc_to_slide_json(&doc);
        assert_eq!(out, sample_json());
        // 再取り込み→再シリアライズも一致（往復保証）。
        let doc2 = Doc::new();
        import_slide_json(&doc2, &out).expect("reimport");
        assert_eq!(doc_to_slide_json(&doc2), out);
    }

    #[test]
    fn インポートで敵対的htmlが落ちる() {
        let dirty = r#"{"version":1,"slides":[{"id":"a","html":"<script>alert(1)</script><p onclick=\"x()\">ok</p>"}]}"#;
        let doc = Doc::new();
        import_slide_json(&doc, dirty).expect("import");
        let out = doc_to_slide_json(&doc);
        assert!(!out.contains("script"));
        assert!(!out.contains("onclick"));
        assert!(out.contains("ok"));
    }

    #[test]
    fn 不正jsonはエラーでドキュメントを壊さない() {
        let doc = Doc::new();
        import_slide_json(&doc, &sample_json()).expect("import");
        let before = doc_to_slide_json(&doc);
        assert!(import_slide_json(&doc, "{broken").is_err());
        assert_eq!(doc_to_slide_json(&doc), before);
    }

    #[test]
    fn normalizeが生htmlを遮断する() {
        let raw = r#"{"version":1,"slides":[{"id":"a","html":"<iframe src=\"https://evil\"></iframe><h1>t</h1>"}]}"#;
        let normalized = normalize_slide_json(raw).expect("normalize");
        assert!(!normalized.contains("iframe"));
        assert!(normalized.contains("<h1>t</h1>"));
    }
}
