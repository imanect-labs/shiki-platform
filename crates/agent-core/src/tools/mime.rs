//! 拡張子→content_type の最小マップ（成果物保存・ワークスペース書込で共用）。

/// 拡張子から content_type を推定する（不明は octet-stream）。
pub(super) fn content_type_for(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or_default() {
        "csv" => "text/csv",
        "json" => "application/json",
        "txt" => "text/plain",
        "md" => "text/markdown",
        "html" => "text/html",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "py" => "text/x-python",
        "js" => "text/javascript",
        "ts" => "text/typescript",
        "toml" => "application/toml",
        "yaml" | "yml" => "application/yaml",
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
    }
}
