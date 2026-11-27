//! 背景地図（回答・ノート・ミニアプリの ```ui の `map` に重ねる地図）の設定（`SHIKI__MAP__*`）。
//!
//! 地図データは shiki-server が自分で配る（ブラウザは shiki 以外へ接続しない・design.md §4.7）。
//! `data_dir` に OpenMapTiles 形式の `basemap.pmtiles` を置くと背景地図が出る。未設定・ファイルが
//! 無いときは地図データなしとして扱い、web は Yui の背景なしの図を描く。
//! データの作り方は `scripts/basemap/build-basemap.sh`。
//!
//! 道順（Yui の `route:`）を道路沿いにするときは、自前で動かす Valhalla の URL を `routing_url` に渡す
//! （shiki-server だけが呼ぶ。ブラウザは Valhalla へ接続しない）。未設定なら道順は Yui と同じ直線。
//! 経路データの作り方は `scripts/basemap/build-routing.sh`。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::ConfigError;

/// 地図データのファイル名（`data_dir` の直下に置く）。名前は固定にして、
/// リクエストから組み立てたパスでファイルを開かない。
pub const BASEMAP_FILE: &str = "basemap.pmtiles";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MapConfig {
    /// 地図データ（`basemap.pmtiles`）を置くディレクトリ。未設定・空文字（空白だけを含む）なら地図データなし。
    #[serde(default, deserialize_with = "super::empty_str_as_none")]
    pub data_dir: Option<String>,
    /// 道路沿いの道順を計算する Valhalla のベース URL（例 `http://valhalla:8002`）。未設定・空文字なら
    /// 道順は直線のまま。
    #[serde(default, deserialize_with = "super::empty_str_as_none")]
    pub routing_url: Option<String>,
}

impl MapConfig {
    /// 地図データのパス（`data_dir` が未設定なら `None`）。ファイルがあるかは見ない
    /// （起動後に置いてもよいので、確かめるのは配るときにする）。
    pub fn basemap_path(&self) -> Option<PathBuf> {
        self.data_dir
            .as_deref()
            .map(|d| PathBuf::from(d.trim()).join(BASEMAP_FILE))
    }

    /// 道順の計算先（末尾の `/` を除いたベース URL）。未設定なら `None`。
    pub fn routing_base(&self) -> Option<&str> {
        self.routing_url
            .as_deref()
            .map(|u| u.trim().trim_end_matches('/'))
    }

    /// 起動時の検証。不正な URL を起動後に潜伏させない。
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if let Some(url) = self.routing_base() {
            let ok = reqwest::Url::parse(url)
                .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some());
            if !ok {
                return Err(ConfigError::Invalid(format!(
                    "map.routing_url が http(s) の URL ではありません: {url}"
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_dir_means_no_basemap() {
        assert_eq!(MapConfig::default().basemap_path(), None);
    }

    #[test]
    fn points_to_fixed_name_under_dir() {
        let c = MapConfig {
            data_dir: Some(" /srv/map ".into()),
            ..MapConfig::default()
        };
        assert_eq!(
            c.basemap_path(),
            Some(PathBuf::from("/srv/map/basemap.pmtiles"))
        );
    }

    #[test]
    fn blank_dir_reads_as_unset() {
        let c: MapConfig = serde_json::from_value(serde_json::json!({ "data_dir": "  " }))
            .expect("map 設定を読めること");
        assert_eq!(c.data_dir, None);
        let c: MapConfig =
            serde_json::from_value(serde_json::json!({})).expect("map 設定を読めること");
        assert_eq!(c.data_dir, None);
    }

    #[test]
    fn routing_url_is_optional_and_validated() {
        assert_eq!(MapConfig::default().routing_base(), None);
        assert!(MapConfig::default().validate().is_ok());
        let c = MapConfig {
            routing_url: Some(" http://valhalla:8002/ ".into()),
            ..MapConfig::default()
        };
        assert_eq!(c.routing_base(), Some("http://valhalla:8002"));
        assert!(c.validate().is_ok());
        for bad in ["valhalla:8002", "ftp://valhalla", "http://"] {
            let c = MapConfig {
                routing_url: Some(bad.into()),
                ..MapConfig::default()
            };
            assert!(c.validate().is_err(), "{bad}");
        }
    }
}
