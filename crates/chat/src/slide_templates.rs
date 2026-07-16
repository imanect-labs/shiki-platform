//! スライドのテーマカタログとデザイン指針（Task 11.3・design §4.8.3）。
//!
//! テーマは **Rust 定数の閉集合**として持ち、ツール定義（save_slide / slide.edit）の
//! description に「変換可能サブセットの語彙」として焼き込む。レンダラ側の適用は既存の
//! bg／インライン style の範囲であり、ここはカタログ定数とプロンプト文字列のみを提供する
//! （エンジン化しない・過剰実装しない）。

use std::sync::LazyLock;

/// スライドテーマ（閉集合）。`theme_id` はメタデータ（`meta.extra` の `theme_id` キー）に載り、
/// `css_hint` はモデルがインライン style で配色を再現するための指針。
pub struct Theme {
    pub id: &'static str,
    pub name: &'static str,
    pub css_hint: &'static str,
}

/// 利用可能テーマの閉集合（未知の theme_id は fail-closed で拒否する）。
pub const THEMES: &[Theme] = &[
    Theme {
        id: "plain",
        name: "白基調",
        css_hint: "背景 #ffffff・本文 #1f2937・見出し #111827・アクセント #2563eb",
    },
    Theme {
        id: "dark",
        name: "ダーク",
        css_hint: "背景 #0f172a・本文 #e2e8f0・見出し #f8fafc・アクセント #38bdf8",
    },
    Theme {
        id: "warm",
        name: "和紙（暖色）",
        css_hint: "背景 #faf6ef・本文 #44403c・見出し #1c1917・アクセント #b45309",
    },
    Theme {
        id: "forest",
        name: "深緑",
        css_hint: "背景 #f4f9f4・本文 #1f2937・見出し #14532d・アクセント #15803d",
    },
];

/// id がカタログにあるか（閉集合の照合・fail-closed）。
pub fn is_known_theme(id: &str) -> bool {
    THEMES.iter().any(|t| t.id == id)
}

/// テーマ一覧＋見栄えの指針のプロンプト断片（ツール description に焼き込む）。
///
/// design §4.8.3「テーマカタログとレイアウトパターンを閉集合で持ち、ツール定義に
/// 変換可能サブセットの語彙として焼き込む」の実装。1280×720 の 16:9 キャンバス・
/// 基本要素サブセット（pptx エクスポート可能な範囲）へモデルを誘導する。
pub fn design_guidance() -> &'static str {
    static GUIDANCE: LazyLock<String> = LazyLock::new(|| {
        use std::fmt::Write as _;
        let mut s = String::from(
            "本文 HTML は 1280×720（16:9）キャンバス前提で、h1/h2/h3/p/ul/li/table/div（イン\
             ライン style）の基本要素だけで構成する（script 等は自動除去される）。見栄えの指針: \
             1 スライド 1 メッセージ・大きな見出し（表紙 h1 は 64px 級、本文スライドは h2 40px \
             級）・箇条書きは 5 点以内・十分な余白（div の padding で確保）・配色はテーマに合わ\
             せてインライン style で指定する。利用可能な theme_id（この閉集合以外は指定不可）: ",
        );
        for (i, t) in THEMES.iter().enumerate() {
            if i > 0 {
                s.push_str(" / ");
            }
            let _ = write!(s, "{}（{}: {}）", t.id, t.name, t.css_hint);
        }
        s.push('。');
        s
    });
    GUIDANCE.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn テーマは閉集合で照合できる() {
        for t in THEMES {
            assert!(is_known_theme(t.id));
        }
        assert!(!is_known_theme("bogus"));
        assert!(!is_known_theme(""));
    }

    #[test]
    fn 指針に全テーマとキャンバス前提が載る() {
        let g = design_guidance();
        for t in THEMES {
            assert!(g.contains(t.id), "theme_id {} が指針に載る", t.id);
        }
        assert!(g.contains("1280×720"));
        assert!(g.contains("箇条書きは 5 点以内"));
    }
}
