//! 地図（回答の ```ui の `map` の背景タイル）の設定（`SHIKI__MAP__*`）。
//!
//! 背景タイルはブラウザが提供元から直接読む「外部接続必須機能」（requirements NFR-2）。
//! `provider` 未指定なら地図は無効で、web は Yui の静止画（位置関係だけ）を描く。
//! クラウド/オンプレの差は provider の値で吸収する（SaaS=Mapbox / オンプレ=自前のスタイル URL）。

use serde::{Deserialize, Serialize};

use super::ConfigError;

/// 地図の背景タイルの提供元。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MapProvider {
    /// Mapbox（公開トークン `pk.*` と Mapbox のスタイル）。
    Mapbox,
    /// MapLibre のスタイル JSON の URL（自前のタイル配信・OpenFreeMap など）。
    Style,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapConfig {
    /// 提供元（未指定・空文字なら地図の背景を出さない。compose の `${VAR:-}` は空文字を渡す）。
    #[serde(default, deserialize_with = "provider_or_empty")]
    pub provider: Option<MapProvider>,
    /// Mapbox の**公開**トークン（`pk.*`）。ブラウザへ渡すため、秘密トークン（`sk.*`）は起動時に弾く。
    /// URL 制限（Mapbox のアカウント側）を掛けて使う。
    #[serde(default)]
    pub mapbox_token: Option<String>,
    /// Mapbox のスタイル（`所有者/スタイル ID`）。ライト用。
    #[serde(default = "default_mapbox_light")]
    pub mapbox_style_light: String,
    /// Mapbox のスタイル（`所有者/スタイル ID`）。ダーク用。
    #[serde(default = "default_mapbox_dark")]
    pub mapbox_style_dark: String,
    /// スタイル JSON の URL（`provider=style` のとき必須）。ライト用。
    #[serde(default)]
    pub style_url_light: Option<String>,
    /// スタイル JSON の URL。ダーク用（省略時はライトと同じ）。
    #[serde(default)]
    pub style_url_dark: Option<String>,
}

impl Default for MapConfig {
    fn default() -> Self {
        Self {
            provider: None,
            mapbox_token: None,
            mapbox_style_light: default_mapbox_light(),
            mapbox_style_dark: default_mapbox_dark(),
            style_url_light: None,
            style_url_dark: None,
        }
    }
}

fn provider_or_empty<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<MapProvider>, D::Error> {
    use serde::de::{Error, IntoDeserializer};
    match Option::<String>::deserialize(d)? {
        None => Ok(None),
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) => MapProvider::deserialize(s.trim().into_deserializer())
            .map(Some)
            .map_err(|e: serde::de::value::Error| D::Error::custom(format!("map.provider: {e}"))),
    }
}

fn default_mapbox_light() -> String {
    "mapbox/light-v11".into()
}

fn default_mapbox_dark() -> String {
    "mapbox/dark-v11".into()
}

/// ブラウザへ渡す地図の設定（検証済み）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapClientConfig {
    pub provider: MapProvider,
    pub style_url_light: String,
    pub style_url_dark: String,
    /// Mapbox の公開トークン（`mapbox://` の URL を書き換えるときに付ける）。
    pub mapbox_token: Option<String>,
}

impl MapConfig {
    /// 起動時の検証。設定ミス（秘密トークン・不正なスタイル）を起動後に潜伏させない。
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        self.client().map(|_| ())
    }

    /// ブラウザへ渡す設定。地図が無効なら `None`。
    pub fn client(&self) -> Result<Option<MapClientConfig>, ConfigError> {
        let Some(provider) = self.provider else {
            return Ok(None);
        };
        match provider {
            MapProvider::Mapbox => {
                // compose の `${VAR:-}` は空文字を渡し得るため、空も未設定として扱う。
                let token = self
                    .mapbox_token
                    .as_deref()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .ok_or_else(|| {
                        ConfigError::Invalid(
                            "map.provider=mapbox には map.mapbox_token が必要です".into(),
                        )
                    })?;
                if !token.starts_with("pk.") {
                    return Err(ConfigError::Invalid(
                        "map.mapbox_token は公開トークン（pk.）にしてください。ブラウザへ渡すため、\
                         秘密トークン（sk.）は使えません"
                            .into(),
                    ));
                }
                if !token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                {
                    return Err(ConfigError::Invalid(
                        "map.mapbox_token に使えない文字があります".into(),
                    ));
                }
                let style = |name: &str, id: &str| -> Result<String, ConfigError> {
                    let ok = id.split_once('/').is_some_and(|(owner, style)| {
                        [owner, style].iter().all(|s| {
                            !s.is_empty()
                                && s.bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                        })
                    });
                    if !ok {
                        return Err(ConfigError::Invalid(format!(
                            "{name} は「所有者/スタイル ID」で指定してください: {id}"
                        )));
                    }
                    Ok(format!(
                        "https://api.mapbox.com/styles/v1/{id}?access_token={token}"
                    ))
                };
                Ok(Some(MapClientConfig {
                    provider,
                    style_url_light: style("map.mapbox_style_light", &self.mapbox_style_light)?,
                    style_url_dark: style("map.mapbox_style_dark", &self.mapbox_style_dark)?,
                    mapbox_token: Some(token.to_string()),
                }))
            }
            MapProvider::Style => {
                let light = self
                    .style_url_light
                    .as_deref()
                    .map(str::trim)
                    .filter(|u| !u.is_empty())
                    .ok_or_else(|| {
                        ConfigError::Invalid(
                            "map.provider=style には map.style_url_light が必要です".into(),
                        )
                    })?;
                let dark = self
                    .style_url_dark
                    .as_deref()
                    .map(str::trim)
                    .filter(|u| !u.is_empty())
                    .unwrap_or(light);
                for (name, url) in [("map.style_url_light", light), ("map.style_url_dark", dark)] {
                    let ok = reqwest::Url::parse(url)
                        .is_ok_and(|u| matches!(u.scheme(), "https" | "http"));
                    if !ok {
                        return Err(ConfigError::Invalid(format!(
                            "{name} が http(s) の URL ではありません: {url}"
                        )));
                    }
                }
                Ok(Some(MapClientConfig {
                    provider,
                    style_url_light: light.to_string(),
                    style_url_dark: dark.to_string(),
                    mapbox_token: None,
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapbox(token: &str) -> MapConfig {
        MapConfig {
            provider: Some(MapProvider::Mapbox),
            mapbox_token: Some(token.into()),
            ..MapConfig::default()
        }
    }

    #[test]
    fn empty_provider_means_disabled() {
        let parse = |v: serde_json::Value| serde_json::from_value::<MapConfig>(v);
        assert_eq!(
            parse(serde_json::json!({ "provider": "" }))
                .ok()
                .and_then(|c| c.provider),
            None
        );
        assert_eq!(
            parse(serde_json::json!({ "provider": "mapbox" }))
                .ok()
                .and_then(|c| c.provider),
            Some(MapProvider::Mapbox)
        );
        assert!(parse(serde_json::json!({ "provider": "google" })).is_err());
    }

    #[test]
    fn disabled_by_default() {
        assert_eq!(MapConfig::default().client().ok().flatten(), None);
    }

    #[test]
    fn mapbox_builds_style_urls_with_public_token() {
        let c = mapbox("pk.abc-123_X").client().ok().flatten();
        let c = c.as_ref();
        assert_eq!(c.map(|c| c.provider), Some(MapProvider::Mapbox));
        assert_eq!(
            c.map(|c| c.style_url_light.as_str()),
            Some("https://api.mapbox.com/styles/v1/mapbox/light-v11?access_token=pk.abc-123_X")
        );
        assert_eq!(
            c.map(|c| c.style_url_dark.as_str()),
            Some("https://api.mapbox.com/styles/v1/mapbox/dark-v11?access_token=pk.abc-123_X")
        );
        assert_eq!(
            c.and_then(|c| c.mapbox_token.as_deref()),
            Some("pk.abc-123_X")
        );
    }

    #[test]
    fn mapbox_rejects_missing_secret_or_odd_token() {
        for token in ["", "  ", "sk.secret", "pk.a&b=c", "pk.a b"] {
            assert!(mapbox(token).validate().is_err(), "{token:?} を通した");
        }
        let none = MapConfig {
            provider: Some(MapProvider::Mapbox),
            ..MapConfig::default()
        };
        assert!(none.validate().is_err());
    }

    #[test]
    fn mapbox_rejects_malformed_style_ids() {
        for id in ["light-v11", "mapbox/", "/x", "a/b/c", "a/b?x=1", "../x"] {
            let c = MapConfig {
                mapbox_style_dark: id.into(),
                ..mapbox("pk.x")
            };
            assert!(c.validate().is_err(), "{id:?} を通した");
        }
    }

    #[test]
    fn style_requires_http_url_and_falls_back_to_light() {
        let c = MapConfig {
            provider: Some(MapProvider::Style),
            style_url_light: Some("https://tiles.example.com/light.json".into()),
            style_url_dark: Some(String::new()),
            ..MapConfig::default()
        };
        let got = c.client().ok().flatten();
        assert_eq!(
            got.map(|c| (c.style_url_dark, c.mapbox_token)),
            Some(("https://tiles.example.com/light.json".into(), None))
        );
        for bad in [
            None,
            Some(""),
            Some("javascript:alert(1)"),
            Some("not a url"),
        ] {
            let c = MapConfig {
                provider: Some(MapProvider::Style),
                style_url_light: bad.map(Into::into),
                ..MapConfig::default()
            };
            assert!(c.validate().is_err(), "{bad:?} を通した");
        }
    }
}
