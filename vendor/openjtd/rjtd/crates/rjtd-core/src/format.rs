use crate::Error;
use crate::compressed_document::is_just_compressed_document;
use crate::container::read_cfb_stream;
use crate::document_text::has_embedded_document_text;
use crate::document_text::{COMPRESSED_DOCUMENT_PATH, DOCUMENT_TEXT_PATH};

const CFB_MAGIC: &[u8; 8] = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileFormat {
    CompoundDocumentText,
    CompoundEmbeddedDocumentText,
    CompoundJustCompressedDocument,
    CompoundUnknown,
    Unknown,
}

impl FileFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CompoundDocumentText => "cfb-document-text",
            Self::CompoundEmbeddedDocumentText => "cfb-embedded-document-text",
            Self::CompoundJustCompressedDocument => "cfb-just-compressed-document",
            Self::CompoundUnknown => "cfb-unknown",
            Self::Unknown => "unknown",
        }
    }
}

pub fn detect_format(data: &[u8]) -> FileFormat {
    if !data.starts_with(CFB_MAGIC) {
        return FileFormat::Unknown;
    }

    match read_cfb_stream(data, DOCUMENT_TEXT_PATH) {
        Ok(_) => FileFormat::CompoundDocumentText,
        Err(Error::NotFound(_)) => detect_compound_without_document_text(data),
        Err(_) => FileFormat::CompoundUnknown,
    }
}

fn detect_compound_without_document_text(data: &[u8]) -> FileFormat {
    match read_cfb_stream(data, COMPRESSED_DOCUMENT_PATH) {
        Ok(stream) if is_just_compressed_document(&stream) => {
            FileFormat::CompoundJustCompressedDocument
        }
        _ if has_embedded_document_text(data) => FileFormat::CompoundEmbeddedDocumentText,
        _ => FileFormat::CompoundUnknown,
    }
}

#[cfg(test)]
mod tests {
    use super::{FileFormat, detect_format};
    use std::io::{Cursor, Write};

    #[test]
    fn detects_unknown_non_cfb_data() {
        assert_eq!(detect_format(b"not cfb"), FileFormat::Unknown);
    }

    #[test]
    fn detects_document_text_cfb() {
        assert_eq!(
            detect_format(&cfb_with_stream("/DocumentText", b"SsmgV.01")),
            FileFormat::CompoundDocumentText
        );
    }

    #[test]
    fn detects_just_compressed_document_cfb() {
        assert_eq!(
            detect_format(&cfb_with_stream(
                "/JSCompDocument",
                b"\x26\0JustCompressedDocument\0-lh5-\0payload",
            )),
            FileFormat::CompoundJustCompressedDocument
        );
    }

    #[test]
    fn detects_embedded_document_text_cfb() {
        let mut embedded = b"SsmgV.01".to_vec();
        embedded.extend_from_slice(&[0x00, 0x1f]);
        for unit in "Note".encode_utf16() {
            embedded.extend_from_slice(&unit.to_be_bytes());
        }

        assert_eq!(
            detect_format(&cfb_with_stream("/JSSlipObject1", &embedded)),
            FileFormat::CompoundEmbeddedDocumentText
        );
    }

    fn cfb_with_stream(path: &str, payload: &[u8]) -> Vec<u8> {
        let mut compound = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        compound
            .create_stream(path)
            .unwrap()
            .write_all(payload)
            .unwrap();
        compound.into_inner().into_inner()
    }
}
