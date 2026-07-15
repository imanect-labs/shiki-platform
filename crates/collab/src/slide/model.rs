//! スライド文書のモデルと正規化 JSON 表現（Task 11.1・design §4.8.3）。
//!
//! ファイルへのシリアライズ形式は次の正規化 JSON（キーは serde_json の BTreeMap で
//! 辞書順に安定・往復保証の対象）:
//!
//! ```json
//! {
//!   "version": 1,
//!   "meta": { "title": "...", "icon": "...", "tags": ["a"], "thread_id": "...", "任意kv": "..." },
//!   "slides": [ { "id": "...", "html": "<div>…</div>", "notes": "...", "bg": { "color": "#fff" } } ]
//! }
//! ```
//!
//! メタデータはノートと同じ [`NoteMeta`]（Yjs Map "meta"）を共有する — メタデータパネル・
//! アシスタントパネル（active_thread_id）のフロント実装をノートと共用するため。

use serde::{Deserialize, Serialize};

use crate::note::frontmatter::NoteMeta;

/// シリアライズ形式のバージョン（後方互換の判定用）。
pub const SLIDE_DOC_VERSION: u32 = 1;

/// スライド 1 枚。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Slide {
    /// 安定 ID（AI 編集・選択コンテキストの参照キー。空なら取り込み時に採番）。
    #[serde(default)]
    pub id: String,
    /// スライド本文 HTML（**書込側でサニタイズ済みが正規形**・PIT-40）。
    #[serde(default)]
    pub html: String,
    /// スピーカーノート（プレーンテキスト）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    /// 背景指定（色・ドライブ画像参照等の不透明 JSON。描画側が既知キーのみ解釈し、
    /// CSS へ流す際に値を検証する — HTML としては扱わない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<serde_json::Value>,
}

/// スライド文書全体（メタ＋スライド列）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SlideDoc {
    pub meta: NoteMeta,
    pub slides: Vec<Slide>,
}

/// JSON ワイヤ表現（meta はフラットな JSON object へ落とす）。
#[derive(Debug, Serialize, Deserialize)]
struct SlideDocJson {
    version: u32,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    meta: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    slides: Vec<Slide>,
}

impl SlideDoc {
    /// 正規化 JSON へシリアライズする（末尾改行つき・人間可読のインデント）。
    pub fn to_json(&self) -> String {
        let json = SlideDocJson {
            version: SLIDE_DOC_VERSION,
            meta: meta_to_json(&self.meta),
            slides: self.slides.clone(),
        };
        let mut out = serde_json::to_string_pretty(&json).unwrap_or_else(|_| "{}".to_string());
        out.push('\n');
        out
    }

    /// JSON からパースする（未知キーは無視・欠損はデフォルト・不正 JSON は Err）。
    ///
    /// **未知の version は fail-closed で Err**: 将来形式のファイルを v1 として読み
    /// 「空デッキとして取り込み→保存で上書き」のデータ喪失を防ぐ（レビュー指摘対応）。
    pub fn from_json(src: &str) -> Result<SlideDoc, serde_json::Error> {
        let json: SlideDocJson = serde_json::from_str(src)?;
        if json.version != SLIDE_DOC_VERSION {
            return Err(serde::de::Error::custom(format!(
                "未対応のスライド形式バージョンです: {}（対応: {SLIDE_DOC_VERSION}）",
                json.version
            )));
        }
        Ok(SlideDoc {
            meta: meta_from_json(&json.meta),
            slides: json.slides,
        })
    }
}

/// [`NoteMeta`] → フラット JSON object（title/icon/tags/thread_id/任意 kv）。
fn meta_to_json(meta: &NoteMeta) -> serde_json::Map<String, serde_json::Value> {
    use serde_json::Value;
    let mut map = serde_json::Map::new();
    if let Some(title) = &meta.title {
        map.insert("title".into(), Value::String(title.clone()));
    }
    if let Some(icon) = &meta.icon {
        map.insert("icon".into(), Value::String(icon.clone()));
    }
    if !meta.tags.is_empty() {
        map.insert(
            "tags".into(),
            Value::Array(meta.tags.iter().cloned().map(Value::String).collect()),
        );
    }
    if let Some(thread_id) = &meta.thread_id {
        map.insert("thread_id".into(), Value::String(thread_id.clone()));
    }
    for (key, value) in &meta.extra {
        map.insert(key.clone(), Value::String(value.clone()));
    }
    map
}

/// フラット JSON object → [`NoteMeta`]（文字列以外の未知値は捨てず文字列化しない＝無視）。
fn meta_from_json(map: &serde_json::Map<String, serde_json::Value>) -> NoteMeta {
    use serde_json::Value;
    let mut meta = NoteMeta::default();
    for (key, value) in map {
        match (key.as_str(), value) {
            ("title", Value::String(s)) => meta.title = Some(s.clone()),
            ("icon", Value::String(s)) => meta.icon = Some(s.clone()),
            ("thread_id", Value::String(s)) => meta.thread_id = Some(s.clone()),
            ("tags", Value::Array(items)) => {
                meta.tags = items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
            }
            (_, Value::String(s)) => {
                meta.extra.insert(key.clone(), s.clone());
            }
            _ => {}
        }
    }
    meta
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json往復で正規形が安定する() {
        let doc = SlideDoc {
            meta: NoteMeta {
                title: Some("提案書".into()),
                tags: vec!["営業".into()],
                ..NoteMeta::default()
            },
            slides: vec![Slide {
                id: "s1".into(),
                html: "<h1>タイトル</h1>".into(),
                notes: "最初に挨拶".into(),
                bg: Some(serde_json::json!({"color": "#ffffff"})),
            }],
        };
        let json = doc.to_json();
        let parsed = SlideDoc::from_json(&json).expect("parse");
        assert_eq!(parsed, doc);
        // 正規形の安定（2 回目のシリアライズが一致）。
        assert_eq!(parsed.to_json(), json);
    }

    #[test]
    fn 欠損フィールドはデフォルトで埋まる() {
        let doc = SlideDoc::from_json(r#"{"version":1,"slides":[{"id":"a"}]}"#).expect("parse");
        assert_eq!(doc.slides.len(), 1);
        assert_eq!(doc.slides[0].html, "");
        assert!(doc.meta.is_empty());
    }

    #[test]
    fn 不正jsonはエラー() {
        assert!(SlideDoc::from_json("{not json").is_err());
    }

    #[test]
    fn 未知バージョンはfail_closed() {
        // 将来形式を v1 として読み「空デッキ→保存で上書き」する事故を防ぐ。
        assert!(SlideDoc::from_json(r#"{"version":2,"slides":[]}"#).is_err());
        assert!(
            SlideDoc::from_json(r#"{"slides":[]}"#).is_err(),
            "version 欠損も拒否"
        );
    }
}
