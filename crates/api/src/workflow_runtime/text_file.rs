//! ファイルを文章として読めるかの判定（ワークフローの storage.read と AI エージェントの窓読み・BUG-101）。
//!
//! 中身を読む前に、MIME と拡張子で PDF・Office・画像・音声・動画・圧縮などを断る（大きな文書を読み込んでから
//! 捨てない）。読んだ後は、UTF-8 として正しく NUL を含まないものだけを文章とする。以前の storage.read は
//! `from_utf8_lossy` で読んだので、PDF や Word を選ぶと文字化けしたまま成功し、後ろの AI に渡っていた。
//!
//! 読む前の判定は次の順（確かな手がかりから使う）:
//!
//! 1. 拡張子が文章の種類（[`TEXT_EXTENSIONS`]）なら、MIME を見ずに中身の確かめ（[`utf8_text`]）に任せる。
//!    MIME はブラウザーが付けた値をそのまま保存しているので、文章のファイルにも文章でない型が付く
//!    （Excel を入れた Windows のブラウザーは .csv に `application/vnd.ms-excel` を付ける）。
//! 2. MIME があり `application/octet-stream` でなければ、MIME で決める（[`binary_mime`]）。
//! 3. MIME が無いか `application/octet-stream` のときだけ、拡張子で決める（[`BINARY_EXTENSIONS`]）。
//!    文章のファイルにも使う拡張子（Graphviz の .dot・PEM やライセンスキーの .key）は表に入れず、中身の
//!    確かめに任せる。

use workflow_engine::nodes::ports::NOT_TEXT_FILE;
use workflow_engine::PortError;

/// 文章でない種類の MIME か（`image/*` などの族と、文書・圧縮の型）。
fn binary_mime(ct: &str) -> bool {
    let ct = ct.to_ascii_lowercase();
    ct.starts_with("image/")
        || ct.starts_with("audio/")
        || ct.starts_with("video/")
        || ct.starts_with("font/")
        || ct.contains("pdf")
        || ct.contains("officedocument")
        || ct.contains("msword")
        || ct.contains("ms-excel")
        || ct.contains("ms-powerpoint")
        || ct.contains("opendocument")
        || ct.contains("zip")
        || ct.contains("x-tar")
        || ct.contains("x-7z")
        || ct.contains("x-rar")
        || ct.contains("gzip")
}

/// 文章の種類の拡張子（MIME より優先する。中身が UTF-8 でなければ読んだ後に断る）。
const TEXT_EXTENSIONS: &[&str] = &[
    "txt", "text", "md", "markdown", "csv", "tsv", "json", "jsonl", "ndjson", "log", "xml", "html",
    "htm", "yaml", "yml", "toml", "ini",
];

/// 文章でない種類の拡張子（MIME が無いか `application/octet-stream` のときの手がかり）。
const BINARY_EXTENSIONS: &[&str] = &[
    "pdf", "doc", "docx", "docm", "dotx", "xls", "xlsx", "xlsm", "xlsb", "ppt", "pptx", "pptm",
    "odt", "ods", "odp", "rtf", "pages", "numbers", "png", "jpg", "jpeg", "gif", "webp", "bmp",
    "tif", "tiff", "heic", "heif", "ico", "psd", "ai", "mp3", "wav", "m4a", "aac", "flac", "ogg",
    "mp4", "mov", "avi", "mkv", "webm", "wmv", "zip", "gz", "tgz", "bz2", "xz", "7z", "rar", "tar",
    "exe", "dll", "bin", "dmg", "iso", "woff", "woff2", "ttf", "otf", "eot", "sqlite", "db",
    "parquet",
];

/// ファイル名の拡張子（先頭の点だけの名前〔.pdf〕は拡張子なし）。
fn extension(name: &str) -> Option<&str> {
    name.rsplit_once('.')
        .and_then(|(stem, ext)| (!stem.is_empty()).then_some(ext))
}

fn listed(table: &[&str], ext: &str) -> bool {
    table.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

/// 中身を読む前に断るファイルか（判定の順はモジュールの説明）。
pub(super) fn is_binary(name: &str, content_type: Option<&str>) -> bool {
    let ext = extension(name);
    if ext.is_some_and(|e| listed(TEXT_EXTENSIONS, e)) {
        return false;
    }
    match content_type.map(str::trim) {
        Some(ct) if !ct.is_empty() && !ct.eq_ignore_ascii_case("application/octet-stream") => {
            binary_mime(ct)
        }
        _ => ext.is_some_and(|e| listed(BINARY_EXTENSIONS, e)),
    }
}

/// 中身を文章として読む（UTF-8 として正しく、NUL を含まないときだけ）。
pub(super) fn utf8_text(bytes: &[u8]) -> Option<&str> {
    std::str::from_utf8(bytes)
        .ok()
        .filter(|t| !t.contains('\0'))
}

/// 種類が文章でないファイルを storage.read で読もうとしたときの誤り（付録 D の N3.5.1）。
pub(super) fn not_text_kind() -> PortError {
    PortError::new(
        NOT_TEXT_FILE,
        "このファイルは文章として読めません。PDF や Word の本文は「AI エージェント」の「文書の本文を読む」ツールで読みます。",
        false,
    )
}

/// 中身が UTF-8 の文章でないファイルを storage.read で読もうとしたときの誤り。
pub(super) fn not_utf8_text() -> PortError {
    PortError::new(
        NOT_TEXT_FILE,
        "このファイルの中身は文章（UTF-8）として読めません。テキストのファイルなら UTF-8 で保存し直してください。PDF や Word の本文は「AI エージェント」の「文書の本文を読む」ツールで読みます。",
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_kinds_are_refused_by_mime_or_extension() {
        // 拡張子が文章の種類でなければ、MIME で決める。
        for ct in [
            "application/pdf",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "application/msword",
            "image/png",
            "application/zip",
            "video/mp4",
        ] {
            assert!(is_binary("契約書", Some(ct)), "{ct}");
            assert!(is_binary("資料.dat", Some(ct)), "{ct}");
        }
        // MIME が分からない（octet-stream・空）ときは拡張子で決める（大小文字は問わない）。
        for name in [
            "契約書.PDF",
            "報告.docx",
            "表.xlsx",
            "写真.jpeg",
            "まとめ.zip",
        ] {
            assert!(is_binary(name, Some("application/octet-stream")), "{name}");
            assert!(is_binary(name, Some("")), "{name}");
            assert!(is_binary(name, None), "{name}");
        }
        for (name, ct) in [
            ("メモ.txt", Some("text/plain")),
            ("readme.md", Some("text/markdown")),
            ("売上.csv", Some("text/csv")),
            ("設定.json", Some("application/json")),
            ("拡張子なし", None),
            (".pdf", None),
            ("notes.log", Some("application/octet-stream")),
        ] {
            assert!(!is_binary(name, ct), "{name}");
        }
    }

    /// 文章の種類の拡張子は MIME より優先する（ブラウザーが付けた型をそのまま保存しているため）。Excel を入れた
    /// Windows のブラウザーは .csv に `application/vnd.ms-excel` を付ける。中身は読んだ後に UTF-8 か確かめる。
    #[test]
    fn text_extensions_win_over_the_browser_mime() {
        for (name, ct) in [
            ("売上.csv", "application/vnd.ms-excel"),
            ("売上.CSV", "application/vnd.ms-excel"),
            ("一覧.tsv", "application/vnd.ms-excel"),
            ("メモ.txt", "application/pdf"),
        ] {
            assert!(!is_binary(name, Some(ct)), "{name} {ct}");
        }
        // 拡張子が文章の種類でなければ、ms-excel の MIME は断る（本物の .xls）。
        assert!(is_binary("売上.xls", Some("application/vnd.ms-excel")));
        assert!(is_binary("売上", Some("application/vnd.ms-excel")));
    }

    /// 文章のファイルにも使う拡張子（.dot・.key）は拡張子で断らず、中身の確かめに任せる。
    #[test]
    fn ambiguous_extensions_are_left_to_the_content() {
        for name in ["graph.dot", "license.key", "server.KEY"] {
            assert!(!is_binary(name, Some("text/plain")), "{name}");
            assert!(!is_binary(name, Some("application/octet-stream")), "{name}");
            assert!(!is_binary(name, None), "{name}");
        }
        // MIME が文章でない種類と言っていれば、MIME で断る（Word の .dot テンプレート）。
        assert!(is_binary("template.dot", Some("application/msword")));
    }

    #[test]
    fn only_valid_utf8_without_nul_is_text() {
        assert_eq!(utf8_text("経費の申請\n".as_bytes()), Some("経費の申請\n"));
        // PDF の先頭・Shift_JIS の「経費」・NUL を含む中身は文章にしない（文字化けで成功させない）。
        assert_eq!(utf8_text(b"%PDF-1.7\n\xe2\xe3\xcf\xd3"), None);
        assert_eq!(utf8_text(b"\x8c\x6f\x94\xef"), None);
        assert_eq!(utf8_text(b"PK\x03\x04\x00\x00"), None);
        assert_eq!(not_text_kind().code, NOT_TEXT_FILE);
        assert!(!not_utf8_text().retryable);
    }
}
