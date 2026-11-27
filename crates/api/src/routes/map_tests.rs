//! `routes::map` のユニットテスト（範囲指定の解釈・版の照合・スタイルの形）。

use super::*;

#[test]
fn parses_single_byte_ranges() {
    assert_eq!(
        parse_range("bytes=0-9", 100),
        RangeSpec::Satisfiable { start: 0, end: 9 }
    );
    assert_eq!(
        parse_range(" bytes= 5 - ", 100),
        RangeSpec::Satisfiable { start: 5, end: 99 }
    );
    assert_eq!(
        parse_range("bytes=-10", 100),
        RangeSpec::Satisfiable { start: 90, end: 99 }
    );
    // 末尾からの長さがファイルより長ければ全体。
    assert_eq!(
        parse_range("bytes=-500", 100),
        RangeSpec::Satisfiable { start: 0, end: 99 }
    );
    // 終わりがファイルを越えたら切り詰める。
    assert_eq!(
        parse_range("bytes=90-1000", 100),
        RangeSpec::Satisfiable { start: 90, end: 99 }
    );
}

#[test]
fn rejects_what_it_does_not_serve() {
    for v in [
        "bytes=100-",    // ファイルの外
        "bytes=10-5",    // 逆順
        "bytes=-0",      // 長さ 0
        "bytes=0-1,4-5", // 複数範囲
        "bytes=a-b",     // 数字でない
        "bytes=+1-2",    // 符号
        "bytes=0",       // 区切りが無い
        "bytes=-",       // 両端が無い
        "lines=0-1",     // 単位が違う
        "",
    ] {
        assert_eq!(parse_range(v, 100), RangeSpec::Unsatisfiable, "{v}");
    }
    // 空のファイルには範囲が無い。
    assert_eq!(parse_range("bytes=0-0", 0), RangeSpec::Unsatisfiable);
}

#[test]
fn caps_the_size_of_one_range() {
    let len = MAX_RANGE_BYTES * 4;
    assert_eq!(
        parse_range(&format!("bytes=0-{}", MAX_RANGE_BYTES - 1), len),
        RangeSpec::Satisfiable {
            start: 0,
            end: MAX_RANGE_BYTES - 1
        }
    );
    assert_eq!(
        parse_range(&format!("bytes=0-{MAX_RANGE_BYTES}"), len),
        RangeSpec::Unsatisfiable
    );
    assert_eq!(parse_range("bytes=0-", len), RangeSpec::Unsatisfiable);
}

#[test]
fn if_match_accepts_current_version_or_wildcard() {
    let etag = "\"10-20\"";
    let mut h = HeaderMap::new();
    assert!(if_match_ok(&h, etag));
    h.insert(header::IF_MATCH, HeaderValue::from_static("\"10-20\""));
    assert!(if_match_ok(&h, etag));
    h.insert(
        header::IF_MATCH,
        HeaderValue::from_static("\"x\", \"10-20\""),
    );
    assert!(if_match_ok(&h, etag));
    h.insert(header::IF_MATCH, HeaderValue::from_static("*"));
    assert!(if_match_ok(&h, etag));
    h.insert(header::IF_MATCH, HeaderValue::from_static("\"old\""));
    assert!(!if_match_ok(&h, etag));
}

#[test]
fn style_reads_nothing_but_the_archive() {
    for theme in [MapTheme::Light, MapTheme::Dark] {
        let style = basemap_style(theme, ARCHIVE_PATH);
        assert_eq!(
            style["sources"]["openmaptiles"]["url"],
            "pmtiles:///map/basemap.pmtiles"
        );
        assert!(style.get("glyphs").is_none() && style.get("sprite").is_none());
        let ids: Vec<&str> = style["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["id"].as_str().unwrap())
            .collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "レイヤの id が重複している");
        assert!(ids.contains(&"place-major") && ids.contains(&"road-major"));
    }
    let attribution = basemap_style(MapTheme::Light, ARCHIVE_PATH)["sources"]["openmaptiles"]
        ["attribution"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(attribution.contains("© OpenStreetMap contributors"));
    assert!(attribution.contains("© OpenMapTiles"));
}
