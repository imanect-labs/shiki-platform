#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    kind: RecordKind,
    payload: Vec<u8>,
}

impl Record {
    pub fn new(kind: RecordKind, payload: Vec<u8>) -> Self {
        Self { kind, payload }
    }

    pub fn kind(&self) -> &RecordKind {
        &self.kind
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordKind {
    Known(u32),
    Unknown(UnknownRecordKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownRecordKind {
    tag: Option<u32>,
}

impl UnknownRecordKind {
    pub fn new(tag: Option<u32>) -> Self {
        Self { tag }
    }

    pub fn tag(&self) -> Option<u32> {
        self.tag
    }
}
