//! `SecretStore` の内部ヘルパ（メタ変換・入力検証・宛先ホスト正規化）。500 行ゲート対応で分離。

use crate::store::{SecretMeta, SecretRow, MAX_NAME_LEN};
use crate::SecretError;

pub(crate) fn to_meta(row: SecretRow) -> SecretMeta {
    SecretMeta {
        id: row.id,
        name: row.name,
        owner: row.owner,
        allowed_hosts: row.allowed_hosts,
        version: row.version,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

pub(crate) fn validate_name(name: &str) -> Result<&str, SecretError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(SecretError::Invalid("name が空です".into()));
    }
    if name.len() > MAX_NAME_LEN {
        return Err(SecretError::Invalid("name が長すぎます".into()));
    }
    Ok(name)
}

/// 宛先ホストを正規化・検証する（小文字化・空要素除去・重複排除）。
pub(crate) fn normalize_hosts(hosts: &[String]) -> Result<Vec<String>, SecretError> {
    let mut out: Vec<String> = Vec::new();
    for h in hosts {
        let h = h.trim().to_ascii_lowercase();
        if h.is_empty() {
            continue;
        }
        // 明らかに不正なホスト（スキーム/パス/空白を含む）は拒否。
        if h.contains('/') || h.contains(' ') || h.contains(':') {
            return Err(SecretError::Invalid(format!("不正な宛先ホスト: {h}")));
        }
        if !out.contains(&h) {
            out.push(h);
        }
    }
    Ok(out)
}
