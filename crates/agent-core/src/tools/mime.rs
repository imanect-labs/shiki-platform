//! 拡張子→content_type の最小マップ（成果物保存・ワークスペース書込で共用）。

/// 拡張子から content_type を推定する（不明は octet-stream）。
pub(super) fn content_type_for(name: &str) -> &'static str {
    let ext = name
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "json" => "application/json",
        "txt" => "text/plain",
        "md" => "text/markdown",
        "html" => "text/html",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "py" => "text/x-python",
        "js" => "text/javascript",
        "ts" => "text/typescript",
        "toml" => "application/toml",
        "yaml" | "yml" => "application/yaml",
        // Office 文書は正しい型で保存しないと Office エンジン（office.read / 編集）・Collabora・
        // 索引がすべて「対応外のファイル」として扱う（拡張子の大小は問わない）。
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odt" => "application/vnd.oasis.opendocument.text",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_common_extensions() {
        assert_eq!(content_type_for("a.csv"), "text/csv");
        assert_eq!(content_type_for("main.py"), "text/x-python");
        assert_eq!(content_type_for("noext"), "application/octet-stream");
        assert_eq!(content_type_for("a.webp"), "image/webp");
    }

    /// Office 成果物が octet-stream になると office.read が「対応外」で拒否する（S2 検証）。
    #[test]
    fn office_documents_get_their_office_types() {
        assert_eq!(
            content_type_for("集計 (3).xlsx"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
        assert_eq!(
            content_type_for("報告.DOCX"),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
        assert!(content_type_for("a.pptx").contains("presentationml"));
        assert_eq!(
            content_type_for("a.ods"),
            "application/vnd.oasis.opendocument.spreadsheet"
        );
    }
}
