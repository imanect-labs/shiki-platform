//! ファイルを文章として読めるかの判定（ワークフローの storage.read と AI エージェントの窓読み・BUG-101）。
//!
//! 中身を読む前に、MIME と拡張子で PDF・Office・画像・音声・動画・圧縮などを断る（大きな文書を読み込んでから
//! 捨てない）。読んだ後は、UTF-8 として正しく NUL を含まないものだけを文章とする。以前の storage.read は
//! `from_utf8_lossy` で読んだので、PDF や Word を選ぶと文字化けしたまま成功し、後ろの AI に渡っていた。

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

/// 文章でない種類の拡張子（MIME が `application/octet-stream` や空のときの手がかり）。
const BINARY_EXTENSIONS: &[&str] = &[
    "pdf", "doc", "docx", "docm", "dot", "dotx", "xls", "xlsx", "xlsm", "xlsb", "ppt", "pptx",
    "pptm", "odt", "ods", "odp", "rtf", "pages", "numbers", "key", "png", "jpg", "jpeg", "gif",
    "webp", "bmp", "tif", "tiff", "heic", "heif", "ico", "psd", "ai", "mp3", "wav", "m4a", "aac",
    "flac", "ogg", "mp4", "mov", "avi", "mkv", "webm", "wmv", "zip", "gz", "tgz", "bz2", "xz",
    "7z", "rar", "tar", "exe", "dll", "bin", "dmg", "iso", "woff", "woff2", "ttf", "otf", "eot",
    "sqlite", "db", "parquet",
];

/// 中身を読む前に断るファイルか（MIME か拡張子が文章でない種類）。
pub(super) fn is_binary(name: &str, content_type: Option<&str>) -> bool {
    if content_type.is_some_and(binary_mime) {
        return true;
    }
    name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && BINARY_EXTENSIONS
                .iter()
                .any(|b| b.eq_ignore_ascii_case(ext))
    })
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
        for ct in [
            "application/pdf",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "application/msword",
            "image/png",
            "application/zip",
            "video/mp4",
        ] {
            assert!(is_binary("a.txt", Some(ct)), "{ct}");
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
