//! 引用の位置情報（issue #508）。
//!
//! 引用は「版ごとの正規化ブロック列（`doc_block`）の中の範囲」で指す。原本（docx の XML・md の
//! バイト列）の位置には戻さない。出典パネルはブロック列をそのまま描くので、形式に関係なく
//! 確実にハイライトできる。元のエディタで開くときは [`TextQuote`] の一節を本文検索して探す。
//!
//! - オフセットは **UTF-16 コード単位**。読むのはブラウザ（JS の文字列・ProseMirror）だけなので、
//!   変換の手間とずれをなくす。Rust 側で数えるときは [`utf16_len`] を使う。
//! - 型はここを正として utoipa → OpenAPI → TS へ流す（手書きミラー禁止）。

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// `doc_block` 上の範囲（両端ともブロック内オフセット・UTF-16）。
///
/// `[block_start:off_start, block_end:off_end)` の半開区間。1 ブロック内なら start と end の
/// ブロックが同じになる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Anchor {
    pub block_start: i32,
    pub off_start: i32,
    pub block_end: i32,
    pub off_end: i32,
}

/// 元エディタで探すための一節（W3C Web Annotation の TextQuoteSelector と同じ考え方）。
///
/// 版が変わってもオフセットに頼らず探し直せる。`exact` は引用したチャンクの本文、
/// `prefix` / `suffix` は前後の文脈（各 [`QUOTE_CONTEXT_CHARS`] 文字まで）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TextQuote {
    pub exact: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub suffix: String,
}

/// 前後の文脈として残す文字数。
pub const QUOTE_CONTEXT_CHARS: usize = 32;

/// PDF のページ上の枠（Docling の prov をそのまま写す）。
///
/// `bbox` は PDF ポイント座標で `[l, t, r, b]`。原点は `origin` が示す（Docling は
/// `BOTTOMLEFT` が既定）。ビューアはページの高さで上下を反転して重ねる。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PageBox {
    pub page: i32,
    pub bbox: [f32; 4],
    #[serde(default = "default_origin")]
    pub origin: BoxOrigin,
}

/// bbox の座標原点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BoxOrigin {
    BottomLeft,
    TopLeft,
}

fn default_origin() -> BoxOrigin {
    BoxOrigin::BottomLeft
}

/// 文字列の UTF-16 長（オフセットの単位をブラウザに合わせる）。
#[must_use]
pub fn utf16_len(s: &str) -> i32 {
    i32::try_from(s.encode_utf16().count()).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_len_counts_surrogate_pairs_as_two() {
        assert_eq!(utf16_len("abc"), 3);
        assert_eq!(utf16_len("第4条"), 3);
        // 𠮷 は BMP 外（サロゲートペア）。JS の length と同じく 2 と数える。
        assert_eq!(utf16_len("𠮷"), 2);
    }

    #[test]
    fn page_box_defaults_origin_to_bottom_left() {
        let b: PageBox = serde_json::from_value(serde_json::json!({
            "page": 2, "bbox": [1.0, 2.0, 3.0, 4.0]
        }))
        .unwrap();
        assert_eq!(b.origin, BoxOrigin::BottomLeft);
    }
}
