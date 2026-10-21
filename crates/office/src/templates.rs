//! 新規 Office ファイルの空テンプレート（#381）。
//!
//! 「新規作成」の実体はこの埋め込みテンプレを StorageService へ書くだけで、本文は
//! **Collabora（LibreOffice）自身に HTML paste で書かせる**（[`crate::live`]）。
//! md→docx の自前サブセット変換を新規作成経路から外すための土台（#381 の決定）。
//!
//! テンプレの正本はここ 1 箇所（ドライブの「新規作成」・AI の作成ツールが共有する）。

/// 作成できる Office ファイルの種別（拡張子・content_type・テンプレを束ねる）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfficeKind {
    /// Word 文書（Collabora Writer）。
    Document,
    /// Excel ブック（Collabora Calc）。
    Spreadsheet,
}

/// Word 文書（.docx）の content_type（[`crate::EDITABLE_CONTENT_TYPES`] の先頭と同値）。
pub const DOCX_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
/// Excel ブック（.xlsx）の content_type。
pub const XLSX_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

/// 空の Word 文書テンプレ（ドライブ「新規作成 > ドキュメント」と同一の正本）。
pub(crate) const BLANK_DOCX: &[u8] = include_bytes!("../templates/blank.docx");
/// 空の Excel ブックテンプレ（シート 1 枚・#381 で追加）。
pub(crate) const BLANK_XLSX: &[u8] = include_bytes!("../templates/blank.xlsx");

impl OfficeKind {
    /// ファイル名に付ける拡張子（先頭ドット無し）。
    pub const fn extension(self) -> &'static str {
        match self {
            OfficeKind::Document => "docx",
            OfficeKind::Spreadsheet => "xlsx",
        }
    }

    /// 保存する content_type。
    pub const fn content_type(self) -> &'static str {
        match self {
            OfficeKind::Document => DOCX_CONTENT_TYPE,
            OfficeKind::Spreadsheet => XLSX_CONTENT_TYPE,
        }
    }

    /// 空テンプレの bytes。
    pub(crate) const fn blank(self) -> &'static [u8] {
        match self {
            OfficeKind::Document => BLANK_DOCX,
            OfficeKind::Spreadsheet => BLANK_XLSX,
        }
    }

    /// 表示名（エラー文・観測テキスト用）。
    pub const fn label(self) -> &'static str {
        match self {
            OfficeKind::Document => "Word 文書",
            OfficeKind::Spreadsheet => "Excel ブック",
        }
    }

    /// 表示名へ拡張子を（大文字小文字を問わず未付与なら）付ける。
    pub fn file_name(self, name: &str) -> String {
        let ext = self.extension();
        if name.to_ascii_lowercase().ends_with(&format!(".{ext}")) {
            name.to_string()
        } else {
            format!("{name}.{ext}")
        }
    }
}

/// 空テンプレの bytes（HTTP の作成エンドポイントが直接書くための公開版）。
///
/// テンプレの正本を api クレートへ複製させないための出口（`include_bytes!` を散らさない）。
pub const fn blank_template(kind: OfficeKind) -> &'static [u8] {
    kind.blank()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 両テンプレとも zip（OOXML）で、種別と content_type/拡張子が対応する。
    #[test]
    fn templates_are_ooxml_zip() {
        for kind in [OfficeKind::Document, OfficeKind::Spreadsheet] {
            assert_eq!(&kind.blank()[..2], b"PK", "{kind:?} は zip で始まる");
            assert!(crate::EDITABLE_CONTENT_TYPES.contains(&kind.content_type()));
        }
        assert_eq!(OfficeKind::Document.content_type(), DOCX_CONTENT_TYPE);
        assert_eq!(OfficeKind::Spreadsheet.content_type(), XLSX_CONTENT_TYPE);
    }

    /// 拡張子は重複付与しない（`.xlsx` / `.XLSX` を渡しても 1 つだけ）。
    #[test]
    fn file_name_appends_extension_once() {
        let k = OfficeKind::Spreadsheet;
        assert_eq!(k.file_name("売上"), "売上.xlsx");
        assert_eq!(k.file_name("売上.xlsx"), "売上.xlsx");
        assert_eq!(k.file_name("売上.XLSX"), "売上.XLSX");
        assert_eq!(OfficeKind::Document.file_name("提案"), "提案.docx");
    }
}
