//! 地図（回答の ```ui の `map` の背景タイル）の設定（`SHIKI__MAP__*`）。
//!
//! 背景タイルはブラウザが配信元から直接読む「外部接続必須機能」（requirements NFR-2）。
//! 配信元は MapLibre のスタイル JSON の URL で指定する（OpenFreeMap・自前のタイル配信など）。
//! 未設定なら地図は無効で、web は Yui の静止画（位置関係だけ）を描く。

use serde::{Deserialize, Serialize};

use super::ConfigError;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MapConfig {
    /// スタイル JSON の URL。ライト用（未設定・空文字なら地図の背景を出さない。
    /// compose の `${VAR:-}` は空文字を渡す）。
    #[serde(default)]
    pub style_url_light: Option<String>,
    /// スタイル JSON の URL。ダーク用（省略時はライトと同じ。配色は web が明暗に合わせて塗り直す）。
    #[serde(default)]
    pub style_url_dark: Option<String>,
}

/// ブラウザへ渡す地図の設定（検証済み）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapClientConfig {
    pub style_url_light: String,
    pub style_url_dark: String,
}

fn non_empty(v: Option<&String>) -> Option<&str> {
    v.map(|s| s.trim()).filter(|s| !s.is_empty())
}

impl MapConfig {
    /// 起動時の検証。不正な URL を起動後に潜伏させない。
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        self.client().map(|_| ())
    }

    /// ブラウザへ渡す設定。地図が無効なら `None`。
    pub fn client(&self) -> Result<Option<MapClientConfig>, ConfigError> {
        let Some(light) = non_empty(self.style_url_light.as_ref()) else {
            return Ok(None);
        };
        let dark = non_empty(self.style_url_dark.as_ref()).unwrap_or(light);
        for (name, url) in [("map.style_url_light", light), ("map.style_url_dark", dark)] {
            let ok = reqwest::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "https" | "http"));
            if !ok {
                return Err(ConfigError::Invalid(format!(
                    "{name} が http(s) の URL ではありません: {url}"
                )));
            }
        }
        Ok(Some(MapClientConfig {
            style_url_light: light.to_string(),
            style_url_dark: dark.to_string(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(light: Option<&str>, dark: Option<&str>) -> MapConfig {
        MapConfig {
            style_url_light: light.map(Into::into),
            style_url_dark: dark.map(Into::into),
        }
    }

    #[test]
    fn disabled_when_unset_or_empty() {
        assert_eq!(MapConfig::default().client().ok().flatten(), None);
        assert_eq!(
            config(Some(" "), Some("https://x/dark.json"))
                .client()
                .ok()
                .flatten(),
            None
        );
    }

    #[test]
    fn dark_falls_back_to_light() {
        let got = config(Some("https://tiles.example.com/light.json"), Some(""))
            .client()
            .ok()
            .flatten();
        assert_eq!(
            got,
            Some(MapClientConfig {
                style_url_light: "https://tiles.example.com/light.json".into(),
                style_url_dark: "https://tiles.example.com/light.json".into(),
            })
        );
    }

    #[test]
    fn rejects_non_http_urls() {
        for bad in ["javascript:alert(1)", "not a url", "file:///etc/passwd"] {
            assert!(
                config(Some(bad), None).validate().is_err(),
                "{bad:?} を通した"
            );
            assert!(
                config(Some("https://ok.example.com/s.json"), Some(bad))
                    .validate()
                    .is_err(),
                "dark {bad:?} を通した"
            );
        }
    }
}
