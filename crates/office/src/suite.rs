//! `OfficeSuite` トレイトと Collabora 実装（Task 11.5・design §4.8）。
//!
//! Office 互換ファイルのブラウザ内編集を担うスイートをトレイト裏に置き、
//! OnlyOffice への差し替え退路を確保する（アプリ本体を分岐させない）。
//! Collabora の discovery（`/hosting/discovery` XML）は初回アクセス時に取得して
//! TTL 付きでキャッシュし、取得失敗は Err＝機能 off の fail-closed とする。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::error::OfficeError;

/// 対応拡張子（Collabora の編集対象・design §4.8）。
pub const SUPPORTED_EXTENSIONS: &[&str] = &["docx", "xlsx", "pptx", "odt", "ods", "odp"];

/// discovery キャッシュの TTL（1 時間）。
const DISCOVERY_TTL: Duration = Duration::from_hours(1);

/// discovery 取得のタイムアウト（Collabora 停止時に編集セッション発行を長く待たせない）。
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);

/// Office スイート（ブラウザ内編集エンジン）の差し替え点。
#[async_trait]
pub trait OfficeSuite: Send + Sync {
    /// スイート名（監査・ログ用の識別子）。
    fn name(&self) -> &'static str;
    /// 拡張子に対応する編集アクション URL（Collabora discovery 由来）を返す。
    ///
    /// 未対応拡張子は `Ok(None)`。discovery の取得・パース失敗は
    /// `Err`（＝機能 off の fail-closed）。
    async fn editor_action_url(&self, ext: &str) -> Result<Option<String>, OfficeError>;
    /// 対応拡張子の一覧（UI の open 分岐・事前判定用）。
    fn supported_extensions(&self) -> &[&'static str];
}

/// 取得済み discovery（拡張子 → 編集アクション urlsrc）。
struct DiscoveryCache {
    urls: HashMap<String, String>,
    fetched_at: Instant,
}

/// Collabora Online 実装（design §4.8・デプロイは Task 11.5 の別成果物）。
pub struct CollaboraSuite {
    base_url: String,
    http: reqwest::Client,
    cache: tokio::sync::RwLock<Option<DiscoveryCache>>,
}

impl CollaboraSuite {
    /// `base_url` は Collabora のルート（例 `http://localhost:9980`）。
    pub fn new(base_url: &str, http: reqwest::Client) -> Self {
        CollaboraSuite {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
            cache: tokio::sync::RwLock::new(None),
        }
    }

    /// テスト用: discovery 取得を伴わずキャッシュ済み状態を作る。
    #[cfg(test)]
    fn with_cached_urls(urls: HashMap<String, String>) -> Self {
        CollaboraSuite {
            base_url: "http://collabora.invalid".into(),
            http: reqwest::Client::new(),
            cache: tokio::sync::RwLock::new(Some(DiscoveryCache {
                urls,
                fetched_at: Instant::now(),
            })),
        }
    }

    /// discovery を取得してキャッシュを更新し、拡張子マップを返す。
    async fn refresh_discovery(&self) -> Result<HashMap<String, String>, OfficeError> {
        let url = format!("{}/hosting/discovery", self.base_url);
        let xml = self
            .http
            .get(&url)
            .timeout(DISCOVERY_TIMEOUT)
            .send()
            .await
            .map_err(|e| OfficeError::Discovery(format!("取得失敗: {e}")))?
            .error_for_status()
            .map_err(|e| OfficeError::Discovery(format!("HTTP エラー: {e}")))?
            .text()
            .await
            .map_err(|e| OfficeError::Discovery(format!("本文読取失敗: {e}")))?;
        let urls = parse_discovery(&xml)?;
        let mut cache = self.cache.write().await;
        *cache = Some(DiscoveryCache {
            urls: urls.clone(),
            fetched_at: Instant::now(),
        });
        Ok(urls)
    }
}

#[async_trait]
impl OfficeSuite for CollaboraSuite {
    fn name(&self) -> &'static str {
        "collabora"
    }

    async fn editor_action_url(&self, ext: &str) -> Result<Option<String>, OfficeError> {
        let ext = ext.to_ascii_lowercase();
        {
            let cache = self.cache.read().await;
            if let Some(c) = cache.as_ref() {
                if c.fetched_at.elapsed() < DISCOVERY_TTL {
                    return Ok(c.urls.get(&ext).cloned());
                }
            }
        }
        let urls = self.refresh_discovery().await?;
        Ok(urls.get(&ext).cloned())
    }

    fn supported_extensions(&self) -> &[&'static str] {
        SUPPORTED_EXTENSIONS
    }
}

/// discovery XML から「拡張子 → 編集アクション urlsrc」を抽出する。
///
/// `<action name="edit" ext="docx" urlsrc="..."/>` のみ採用する（view は WOPI ホスト側で
/// `UserCanWrite=false` により読み取り専用化されるため、URL は edit で統一する）。
/// 対応拡張子（[`SUPPORTED_EXTENSIONS`]）以外は無視する。
fn parse_discovery(xml: &str) -> Result<HashMap<String, String>, OfficeError> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut urls: HashMap<String, String> = HashMap::new();
    loop {
        match reader.read_event() {
            Ok(Event::Empty(e) | Event::Start(e)) if e.name().as_ref() == b"action" => {
                let mut name = None;
                let mut ext = None;
                let mut urlsrc = None;
                for attr in e.attributes() {
                    let attr =
                        attr.map_err(|e| OfficeError::Discovery(format!("属性不正: {e}")))?;
                    let value = attr
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map_err(|e| OfficeError::Discovery(format!("属性値不正: {e}")))?
                        .into_owned();
                    match attr.key.as_ref() {
                        b"name" => name = Some(value),
                        b"ext" => ext = Some(value.to_ascii_lowercase()),
                        b"urlsrc" => urlsrc = Some(value),
                        _ => {}
                    }
                }
                if let (Some(name), Some(ext), Some(urlsrc)) = (name, ext, urlsrc) {
                    if name == "edit" && SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
                        urls.insert(ext, urlsrc);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(OfficeError::Discovery(format!("XML パース失敗: {e}"))),
        }
    }
    if urls.is_empty() {
        // 空 discovery は Collabora 側の異常（バージョン不整合等）。編集 URL を
        // 返せないため fail-closed で明示エラーにする。
        return Err(OfficeError::Discovery(
            "編集アクションが 1 件も見つかりません".into(),
        ));
    }
    Ok(urls)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// Collabora の discovery を模した固定フィクスチャ（実物の構造・属性順に準拠）。
    const FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<wopi-discovery>
  <net-zone name="external-http">
    <app name="writer">
      <action default="true" ext="odt" name="edit" urlsrc="http://localhost:9980/browser/abc/cool.html?"/>
      <action ext="docx" name="edit" urlsrc="http://localhost:9980/browser/abc/cool.html?"/>
      <action ext="rtf" name="edit" urlsrc="http://localhost:9980/browser/abc/cool.html?"/>
    </app>
    <app name="calc">
      <action default="true" ext="ods" name="edit" urlsrc="http://localhost:9980/browser/abc/cool.html?"/>
      <action ext="xlsx" name="edit" urlsrc="http://localhost:9980/browser/abc/cool.html?"/>
    </app>
    <app name="impress">
      <action default="true" ext="odp" name="edit" urlsrc="http://localhost:9980/browser/abc/cool.html?"/>
      <action ext="pptx" name="edit" urlsrc="http://localhost:9980/browser/abc/cool.html?"/>
    </app>
    <app name="application/pdf">
      <action default="true" ext="pdf" name="view" urlsrc="http://localhost:9980/browser/abc/cool.html?"/>
    </app>
  </net-zone>
</wopi-discovery>"#;

    #[test]
    fn parse_discovery_extracts_edit_urls() {
        let urls = parse_discovery(FIXTURE).unwrap();
        for ext in ["docx", "xlsx", "pptx", "odt", "ods", "odp"] {
            assert!(urls.contains_key(ext), "{ext} の編集 URL が要る");
        }
        // view のみの pdf・対応外の rtf は載らない。
        assert!(!urls.contains_key("pdf"));
        assert!(!urls.contains_key("rtf"));
    }

    #[test]
    fn parse_discovery_rejects_broken_xml() {
        // 未閉タグ: XML エラーか「編集アクション 0 件」のどちらの経路でも fail-closed。
        assert!(parse_discovery("<wopi-discovery><net-zone>").is_err());
        assert!(matches!(
            parse_discovery("<wopi-discovery/>"),
            Err(OfficeError::Discovery(_))
        ));
    }

    #[tokio::test]
    async fn editor_action_url_uses_cache() {
        let urls = parse_discovery(FIXTURE).unwrap();
        let suite = CollaboraSuite::with_cached_urls(urls);
        let got = suite.editor_action_url("DOCX").await.unwrap();
        assert!(got.is_some(), "大文字拡張子も解決される");
        assert_eq!(suite.editor_action_url("exe").await.unwrap(), None);
        assert_eq!(suite.name(), "collabora");
        assert_eq!(suite.supported_extensions(), SUPPORTED_EXTENSIONS);
    }
}
