use crate::container::read_cfb_stream;
use crate::{Error, Result};

pub const DOCUMENT_TEXT_POSITION_TABLES_PATH: &str = "/DocumentTextPositionTables";

const DOCUMENT_TEXT_POSITION_MAGIC: &[u8; 8] = b"SsmgV.01";
const TEXT_COUNT_TABLE_MAGIC: &[u8; 8] = b"TCntV.01";
const TEXT_COUNT_ENTRY_BYTES: usize = 29;
const MARK_TABLE_MAGIC: &[u8; 8] = b"MarkV.01";
const MARK_TABLE_HEADER_BYTES: usize = 6;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentTextPositionTables {
    entries: Vec<DocumentTextPositionEntry>,
    text_count_header: Option<DocumentTextCountHeader>,
    text_count_entries: Vec<DocumentTextCountEntry>,
}

impl DocumentTextPositionTables {
    fn new(
        entries: Vec<DocumentTextPositionEntry>,
        text_count_header: Option<DocumentTextCountHeader>,
        text_count_entries: Vec<DocumentTextCountEntry>,
    ) -> Self {
        Self {
            entries,
            text_count_header,
            text_count_entries,
        }
    }

    pub fn entries(&self) -> &[DocumentTextPositionEntry] {
        &self.entries
    }

    pub fn text_count_header(&self) -> Option<DocumentTextCountHeader> {
        self.text_count_header
    }

    pub fn text_count_entries(&self) -> &[DocumentTextCountEntry] {
        &self.text_count_entries
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentTextPositionEntry {
    id: u16,
    offset: u32,
}

impl DocumentTextPositionEntry {
    fn new(id: u16, offset: u32) -> Self {
        Self { id, offset }
    }

    pub fn id(&self) -> u16 {
        self.id
    }

    pub fn offset(&self) -> u32 {
        self.offset
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentTextCountHeader {
    kind: u16,
    reserved: u16,
    declared_count: u16,
    entries_offset: u16,
}

impl DocumentTextCountHeader {
    fn new(kind: u16, reserved: u16, declared_count: u16, entries_offset: u16) -> Self {
        Self {
            kind,
            reserved,
            declared_count,
            entries_offset,
        }
    }

    pub fn kind(&self) -> u16 {
        self.kind
    }

    pub fn reserved(&self) -> u16 {
        self.reserved
    }

    pub fn declared_count(&self) -> u16 {
        self.declared_count
    }

    pub fn entries_offset(&self) -> u16 {
        self.entries_offset
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentTextCountEntry {
    index: usize,
    start_offset: u32,
    end_offset: u32,
    raw: [u8; TEXT_COUNT_ENTRY_BYTES],
}

impl DocumentTextCountEntry {
    fn new(index: usize, raw: [u8; TEXT_COUNT_ENTRY_BYTES]) -> Self {
        let start_offset = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
        let end_offset = u32::from_be_bytes([raw[4], raw[5], raw[6], raw[7]]);
        Self {
            index,
            start_offset,
            end_offset,
            raw,
        }
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn start_offset(&self) -> u32 {
        self.start_offset
    }

    pub fn end_offset(&self) -> u32 {
        self.end_offset
    }

    pub fn raw(&self) -> &[u8; TEXT_COUNT_ENTRY_BYTES] {
        &self.raw
    }
}

pub fn read_document_text_position_tables(data: &[u8]) -> Result<DocumentTextPositionTables> {
    let stream = read_cfb_stream(data, DOCUMENT_TEXT_POSITION_TABLES_PATH)?;
    parse_document_text_position_tables(&stream)
}

pub fn parse_document_text_position_tables(data: &[u8]) -> Result<DocumentTextPositionTables> {
    if !data.starts_with(DOCUMENT_TEXT_POSITION_MAGIC) {
        return Err(Error::InvalidData(
            "DocumentTextPositionTables missing SsmgV.01 magic".into(),
        ));
    }

    let (text_count_header, text_count_entries) = parse_text_count_table(data);
    let entries = find_magic(data, MARK_TABLE_MAGIC)
        .map(|mark_table_start| parse_mark_table(data, mark_table_start))
        .unwrap_or_default();

    Ok(DocumentTextPositionTables::new(
        entries,
        text_count_header,
        text_count_entries,
    ))
}

fn parse_mark_table(data: &[u8], mark_table_start: usize) -> Vec<DocumentTextPositionEntry> {
    let mut index = mark_table_start + MARK_TABLE_MAGIC.len() + MARK_TABLE_HEADER_BYTES;
    let mut entries = Vec::new();

    while index + 2 <= data.len() {
        let id = read_u16_be(data, index);
        if id == 0xffff {
            break;
        }

        if index + 6 > data.len() {
            break;
        }

        let offset = read_u32_be(data, index + 2);
        if offset == 0xffff_ffff {
            break;
        }

        entries.push(DocumentTextPositionEntry::new(id, offset));
        index += 6;
    }

    entries
}

fn parse_text_count_table(
    data: &[u8],
) -> (Option<DocumentTextCountHeader>, Vec<DocumentTextCountEntry>) {
    let Some(table_start) = find_magic(data, TEXT_COUNT_TABLE_MAGIC) else {
        return (None, Vec::new());
    };
    let header_start = table_start + TEXT_COUNT_TABLE_MAGIC.len();
    if data
        .get(header_start + 2..header_start + 2 + MARK_TABLE_MAGIC.len())
        .is_some_and(|bytes| bytes == MARK_TABLE_MAGIC)
    {
        return (None, Vec::new());
    }
    if header_start + 8 > data.len() {
        return (None, Vec::new());
    }

    let header = DocumentTextCountHeader::new(
        read_u16_be(data, header_start),
        read_u16_be(data, header_start + 2),
        read_u16_be(data, header_start + 4),
        read_u16_be(data, header_start + 6),
    );
    let mut entries = Vec::new();
    let mut index = header.entries_offset() as usize;
    if index < header_start + 8 || index > data.len() {
        return (Some(header), entries);
    }
    for entry_index in 0..header.declared_count() as usize {
        if index + TEXT_COUNT_ENTRY_BYTES > data.len() {
            break;
        }
        let mut raw = [0; TEXT_COUNT_ENTRY_BYTES];
        raw.copy_from_slice(&data[index..index + TEXT_COUNT_ENTRY_BYTES]);
        entries.push(DocumentTextCountEntry::new(entry_index, raw));
        index += TEXT_COUNT_ENTRY_BYTES;
    }

    (Some(header), entries)
}

fn find_magic(data: &[u8], magic: &[u8]) -> Option<usize> {
    data.windows(magic.len()).position(|window| window == magic)
}

fn read_u16_be(data: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([data[offset], data[offset + 1]])
}

fn read_u32_be(data: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

#[cfg(test)]
mod tests {
    use super::{
        DOCUMENT_TEXT_POSITION_TABLES_PATH, parse_document_text_position_tables,
        read_document_text_position_tables,
    };
    use std::io::{Cursor, Write};

    #[test]
    fn parses_mark_position_entries() {
        let table = parse_document_text_position_tables(&position_table_fixture()).unwrap();

        assert_eq!(table.entries().len(), 2);
        assert_eq!(table.entries()[0].id(), 1);
        assert_eq!(table.entries()[0].offset(), 0x1234);
        assert_eq!(table.entries()[1].id(), 2);
        assert_eq!(table.entries()[1].offset(), 0x5678);
        assert!(table.text_count_header().is_none());
        assert!(table.text_count_entries().is_empty());
    }

    #[test]
    fn reads_mark_position_entries_from_cfb() {
        let bytes = cfb_with_position_table(&position_table_fixture());
        let table = read_document_text_position_tables(&bytes).unwrap();

        assert_eq!(table.entries().len(), 2);
        assert_eq!(table.entries()[1].offset(), 0x5678);
    }

    #[test]
    fn rejects_stream_without_document_text_position_magic() {
        let error = parse_document_text_position_tables(b"not a table").unwrap_err();

        assert!(error.to_string().contains("missing SsmgV.01 magic"));
    }

    #[test]
    fn parses_text_count_entries_without_mark_table() {
        let table = parse_document_text_position_tables(&text_count_table_fixture()).unwrap();

        let header = table.text_count_header().unwrap();
        assert_eq!(header.kind(), 1);
        assert_eq!(header.reserved(), 0);
        assert_eq!(header.declared_count(), 2);
        assert_eq!(header.entries_offset(), 36);
        assert!(table.entries().is_empty());
        assert_eq!(table.text_count_entries().len(), 2);
        assert_eq!(table.text_count_entries()[0].index(), 0);
        assert_eq!(table.text_count_entries()[0].start_offset(), 0x1234);
        assert_eq!(table.text_count_entries()[0].end_offset(), 0x1250);
        assert_eq!(table.text_count_entries()[1].index(), 1);
        assert_eq!(table.text_count_entries()[1].start_offset(), 0x2000);
        assert_eq!(table.text_count_entries()[1].end_offset(), 0x2400);
    }

    fn position_table_fixture() -> Vec<u8> {
        let mut bytes = b"SsmgV.01".to_vec();
        bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        bytes.extend_from_slice(&[0x00, 0x00, 0x01, 0x00]);
        bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        bytes.extend_from_slice(b"TCntV.01");
        bytes.extend_from_slice(&[0x00, 0x00]);
        bytes.extend_from_slice(b"MarkV.01");
        bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x02]);
        bytes.extend_from_slice(&[0x00, 0x01, 0x00, 0x00, 0x12, 0x34]);
        bytes.extend_from_slice(&[0x00, 0x02, 0x00, 0x00, 0x56, 0x78]);
        bytes.extend_from_slice(&[0xff, 0xff, 0xff, 0xff]);
        bytes
    }

    fn text_count_table_fixture() -> Vec<u8> {
        let mut bytes = b"SsmgV.01".to_vec();
        bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        bytes.extend_from_slice(&[0x00, 0x00, 0x01, 0x00]);
        bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        bytes.extend_from_slice(b"TCntV.01");
        bytes.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
        bytes.extend_from_slice(&[0x00, 0x02, 0x00, 0x24]);
        let mut first = [0; 29];
        first[0..4].copy_from_slice(&0x1234u32.to_be_bytes());
        first[4..8].copy_from_slice(&0x1250u32.to_be_bytes());
        first[8..12].copy_from_slice(&[0x01, 0x01, 0x00, 0x05]);
        let mut second = [0; 29];
        second[0..4].copy_from_slice(&0x2000u32.to_be_bytes());
        second[4..8].copy_from_slice(&0x2400u32.to_be_bytes());
        second[8..12].copy_from_slice(&[0x02, 0x02, 0x00, 0x07]);
        bytes.extend_from_slice(&first);
        bytes.extend_from_slice(&second);
        bytes
    }

    fn cfb_with_position_table(payload: &[u8]) -> Vec<u8> {
        let mut compound = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        compound
            .create_stream(DOCUMENT_TEXT_POSITION_TABLES_PATH)
            .unwrap()
            .write_all(payload)
            .unwrap();
        compound.into_inner().into_inner()
    }
}
