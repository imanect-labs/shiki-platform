//! Yjs ドキュメント種の閉集合（design §4.8.1）。
//!
//! collab の永続化（update log / snapshot）・authz・圧縮は種別非依存で共有し、
//! **ファイルへのシリアライズ形式と外部書込インポートだけ**を種別で差し替える。
//! 新しいドキュメント種はここに追加する（判定は拡張子・fail-closed: 未知拡張子は
//! None ＝ファイル保存を持たない一時セッション扱い）。

/// collab がファイル保存（シリアライズ/インポート）を持つドキュメント種。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    /// ノート（`.md`・真実=Yjs・md はシリアライズ形式。Task 11P.2）。
    Note,
    /// スライド（`.slide`・真実=Yjs・正規化 JSON はシリアライズ形式。Task 11.1）。
    Slide,
}

/// スライドファイルの MIME（design §4.8.3）。
pub const SLIDE_MIME: &str = "application/vnd.shiki.slide+json";

impl DocKind {
    /// ファイル名からドキュメント種を判定する（大文字小文字を無視・未知拡張子は None）。
    pub fn from_name(name: &str) -> Option<DocKind> {
        let ext = std::path::Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        match ext.as_deref() {
            Some("md") => Some(DocKind::Note),
            Some("slide") => Some(DocKind::Slide),
            _ => None,
        }
    }

    /// 保存時の Content-Type。
    pub fn content_type(self) -> &'static str {
        match self {
            DocKind::Note => "text/markdown",
            DocKind::Slide => SLIDE_MIME,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 拡張子で種別を判定する() {
        assert_eq!(DocKind::from_name("メモ.md"), Some(DocKind::Note));
        assert_eq!(DocKind::from_name("Deck.SLIDE"), Some(DocKind::Slide));
        assert_eq!(DocKind::from_name("data.csv"), None);
        assert_eq!(DocKind::from_name("slide"), None);
    }
}
