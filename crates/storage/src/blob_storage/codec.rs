//! 永続形式 v1。物理オブジェクトも内容ハッシュで検証し、展開量を元サイズで制限する。
use std::{collections::BTreeMap, io::Read};

use serde::{Deserialize, Serialize};

use crate::{content_address::sha256_hex, StorageError};

pub(super) const MAX_BYTES: usize = 64 * 1024 * 1024;
pub(super) const MAX_REFS: usize = MAX_BYTES / 4096 + 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    pub format: u8,
    pub size: usize,
    pub body: Body,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "codec", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Body {
    Full {
        object: String,
    },
    Dictionary {
        object: String,
        base: String,
        base_size: usize,
    },
    Chunks {
        objects: Vec<Chunk>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Chunk {
    pub object: String,
    pub size: usize,
}

pub(super) struct Plan {
    pub manifest: Manifest,
    pub objects: BTreeMap<String, Vec<u8>>,
    pub stored_bytes: usize,
}

fn codec_error(e: impl std::fmt::Display) -> StorageError {
    StorageError::Integrity(format!("保存表現 v1: {e}"))
}

pub(super) fn pack(data: &[u8]) -> Result<Vec<u8>, StorageError> {
    let mut encoder = zstd::bulk::Compressor::new(3).map_err(codec_error)?;
    encoder.include_checksum(true).map_err(codec_error)?;
    let compressed = encoder.compress(data).map_err(codec_error)?;
    let (tag, value) = if compressed.len() < data.len() {
        (b'Z', compressed.as_slice())
    } else {
        (b'R', data)
    };
    let mut out = Vec::with_capacity(value.len() + 1);
    out.push(tag);
    out.extend_from_slice(value);
    Ok(out)
}

pub(super) fn unpack(data: &[u8], size: usize) -> Result<Vec<u8>, StorageError> {
    if size > MAX_BYTES {
        return Err(codec_error("展開サイズ上限"));
    }
    let (tag, value) = data.split_first().ok_or_else(|| codec_error("空の実体"))?;
    let result = match tag {
        b'R' => value.to_vec(),
        b'Z' => {
            let decoder = zstd::stream::read::Decoder::new(value).map_err(codec_error)?;
            let mut result = Vec::new();
            decoder
                .take(size as u64 + 1)
                .read_to_end(&mut result)
                .map_err(codec_error)?;
            result
        }
        _ => return Err(codec_error("未知の実体形式")),
    };
    if result.len() != size {
        return Err(codec_error("展開サイズ不一致"));
    }
    Ok(result)
}

pub(super) fn dictionary_decode(
    data: &[u8],
    base: &[u8],
    size: usize,
) -> Result<Vec<u8>, StorageError> {
    if size > MAX_BYTES {
        return Err(codec_error("辞書展開サイズ上限"));
    }
    let mut decoder = zstd::bulk::Decompressor::with_dictionary(base).map_err(codec_error)?;
    let out = decoder.decompress(data, size).map_err(codec_error)?;
    if out.len() != size {
        return Err(codec_error("辞書展開サイズ不一致"));
    }
    Ok(out)
}

impl Manifest {
    pub(super) fn refs(&self) -> Vec<&str> {
        match &self.body {
            Body::Full { object } => vec![object],
            Body::Dictionary { object, base, .. } => vec![object, base],
            Body::Chunks { objects } => objects.iter().map(|c| c.object.as_str()).collect(),
        }
    }

    pub(super) fn validate(&self, size: usize) -> Result<(), StorageError> {
        if self.format != 1 || self.size != size || size > MAX_BYTES {
            return Err(codec_error("manifest の版/サイズが不正"));
        }
        let refs = self.refs();
        if refs.len() > MAX_REFS
            || refs
                .iter()
                .any(|s| !crate::content_address::is_valid_sha256_hex(s))
        {
            return Err(codec_error("manifest の参照が不正"));
        }
        match &self.body {
            Body::Chunks { objects } => {
                let sum = objects.iter().try_fold(0usize, |n, c| {
                    if c.size == 0 || c.size > 65536 {
                        return None;
                    }
                    n.checked_add(c.size)
                });
                if sum != Some(size) {
                    return Err(codec_error("断片のサイズが不正"));
                }
            }
            Body::Dictionary { base_size, .. } if *base_size > MAX_BYTES => {
                return Err(codec_error("基準版が大きすぎる"))
            }
            _ => {}
        }
        Ok(())
    }
}

/// 基準は常に独立した Full オブジェクト。DAG を再帰的に辿る形式は作らない。
pub(super) fn candidates(
    data: &[u8],
    anchor: Option<(String, Vec<u8>)>,
) -> Result<Vec<Plan>, StorageError> {
    if data.len() > MAX_BYTES {
        return Err(codec_error("最適化サイズ上限"));
    }
    let full = pack(data)?;
    let full_sha = sha256_hex(&full);
    let full_plan = Plan {
        manifest: Manifest {
            format: 1,
            size: data.len(),
            body: Body::Full {
                object: full_sha.clone(),
            },
        },
        stored_bytes: full.len(),
        objects: BTreeMap::from([(full_sha, full)]),
    };
    let mut plans = vec![full_plan];
    if let Some((base_sha, base)) = anchor {
        let mut encoder = zstd::bulk::Compressor::with_dictionary(3, &base).map_err(codec_error)?;
        encoder.include_checksum(true).map_err(codec_error)?;
        let patch = encoder.compress(data).map_err(codec_error)?;
        let sha = sha256_hex(&patch);
        let candidate = Plan {
            manifest: Manifest {
                format: 1,
                size: data.len(),
                body: Body::Dictionary {
                    object: sha.clone(),
                    base: base_sha,
                    base_size: base.len(),
                },
            },
            stored_bytes: patch.len(),
            objects: BTreeMap::from([(sha, patch)]),
        };
        plans.push(candidate);
    }
    if data.len() >= 65536 {
        let mut objects = BTreeMap::new();
        let mut chunks = Vec::new();
        for c in fastcdc::v2020::FastCDC::new(data, 4096, 16384, 65536) {
            let payload = pack(&data[c.offset..c.offset + c.length])?;
            let sha = sha256_hex(&payload);
            chunks.push(Chunk {
                object: sha.clone(),
                size: c.length,
            });
            objects.insert(sha, payload);
        }
        let candidate = Plan {
            manifest: Manifest {
                format: 1,
                size: data.len(),
                body: Body::Chunks { objects: chunks },
            },
            stored_bytes: objects.values().map(Vec::len).sum(),
            objects,
        };
        plans.push(candidate);
    }
    for p in &plans {
        p.manifest.validate(data.len())?;
    }
    Ok(plans)
}

#[cfg(test)]
mod tests;
