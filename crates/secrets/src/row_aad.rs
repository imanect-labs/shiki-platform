//! シークレット暗号文の**行への束縛**（AES-256-GCM の AAD・#814）。
//!
//! 平文は DEK で暗号化し、その AAD に「暗号文が属する行」の識別子を入れる。こうすると DB へ
//! 書込める攻撃者が暗号素材（`ciphertext`/`nonce`/`encrypted_dek`/`dek_nonce`）をまとめて
//! **別テナント・別 org・別 secret の行へ複製しても復号に失敗する**（認証タグ不一致）。
//! DEK の包み（[`KeyProvider`](crate::KeyProvider)）は行を知らないため束縛しない＝包んだ DEK は
//! 複製先でも解けるが、平文側の AAD で止まる。
//!
//! # 暗号化方式の版（`secret.enc_version` 列）
//!
//! | 版 | AAD | 扱い |
//! |---|---|---|
//! | 0 (`Legacy`) | なし | #814 以前の行。**読み出しのみ**後方互換で復号し、新版へ再暗号化する |
//! | 1 (`RowBound`) | `tenant_id`・`org`・`secret_id`（＋版ラベル） | 新規書込・ローテーションは常にこれ |
//!
//! 版ラベルを AAD に含めるので、v1 の暗号文の `enc_version` を 0 へ書き換える**格下げ**は
//! 「AAD なしで開く」ことになり失敗する。未知の版は fail-closed で復号しない。
//!
//! 束縛する識別子は行内で不変のものに限る: `tenant_id`・`org`・`id`。参照名（rename の余地）や
//! ローテーション世代 `version`（攻撃者は列ごと書き戻せるため束縛しても巻き戻しを防げない）は入れない。
//! ⚠️ 将来 `shiki-admin retenant` が `secret` を移行対象にする（現状は副産物未移送で拒否・
//! `storage::tenant_scope::RETENANT_SIDECAR_COMPLETE`）場合は、`tenant_id` を書き換える前後で
//! 復号→新 tenant_id で再暗号化する必要がある。

use uuid::Uuid;

use crate::crypto::{self, CryptoError, Sealed};

/// 暗号化方式の版（DB の `secret.enc_version`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EncVersion {
    /// AAD なし（#814 以前）。復号のみ許し、書込には使わない。
    Legacy,
    /// 行識別子を AAD に束縛（#814）。
    RowBound,
}

impl EncVersion {
    /// 新規書込・ローテーション・再暗号化で使う版。
    pub(crate) const CURRENT: EncVersion = EncVersion::RowBound;

    /// DB 値から版を得る。未知の値は `Decrypt` で fail-closed（復号を試みない）。
    pub(crate) fn from_db(v: i16) -> Result<Self, CryptoError> {
        match v {
            0 => Ok(EncVersion::Legacy),
            1 => Ok(EncVersion::RowBound),
            _ => Err(CryptoError::Decrypt),
        }
    }

    /// DB へ書く値。
    pub(crate) fn as_db(self) -> i16 {
        match self {
            EncVersion::Legacy => 0,
            EncVersion::RowBound => 1,
        }
    }
}

/// 暗号文が属する行の識別子（AAD の素材）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct RowBinding<'a> {
    pub tenant_id: &'a str,
    pub org: &'a str,
    pub id: Uuid,
}

/// v1 の AAD のドメイン分離ラベル（版を含む）。
const AAD_LABEL_V1: &[u8] = b"shiki.secret.row.v1\0";

impl RowBinding<'_> {
    /// 版に応じた AAD を組む。可変長フィールドは長さ前置で連結し、区切り文字の曖昧さ
    /// （`tenant="a|b",org="c"` と `tenant="a",org="b|c"` が同じ列になる）を作らない。
    fn aad(&self, version: EncVersion) -> Vec<u8> {
        match version {
            EncVersion::Legacy => Vec::new(),
            EncVersion::RowBound => {
                let mut out = Vec::with_capacity(
                    AAD_LABEL_V1.len() + 8 + self.tenant_id.len() + self.org.len() + 16,
                );
                out.extend_from_slice(AAD_LABEL_V1);
                push_len_prefixed(&mut out, self.tenant_id.as_bytes());
                push_len_prefixed(&mut out, self.org.as_bytes());
                out.extend_from_slice(self.id.as_bytes());
                out
            }
        }
    }

    /// 現行版（[`EncVersion::CURRENT`]）で行に束縛して暗号化する。
    pub(crate) fn seal(&self, dek: &[u8], plaintext: &[u8]) -> Result<Sealed, CryptoError> {
        crypto::seal(dek, plaintext, &self.aad(EncVersion::CURRENT))
    }

    /// 行の版（`enc_version`）に従って復号する。行の識別子が暗号化時と違えば失敗する。
    pub(crate) fn open(
        &self,
        dek: &[u8],
        enc_version: i16,
        nonce: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        let version = EncVersion::from_db(enc_version)?;
        crypto::open(dek, nonce, ciphertext, &self.aad(version))
    }
}

fn push_len_prefixed(out: &mut Vec<u8>, field: &[u8]) {
    // tenant_id / org は短い識別子。u32::MAX 超は Postgres の text 上限（1GB）を超えるため到達しない。
    let len = u32::try_from(field.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(field);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::generate_key;

    fn row<'a>(tenant_id: &'a str, org: &'a str, id: Uuid) -> RowBinding<'a> {
        RowBinding { tenant_id, org, id }
    }

    #[test]
    fn roundtrip_on_same_row() {
        let dek = generate_key();
        let id = Uuid::new_v4();
        let r = row("t1", "acme", id);
        let sealed = r.seal(&dek, b"xoxb-secret").expect("seal");
        let opened = r
            .open(
                &dek,
                EncVersion::CURRENT.as_db(),
                &sealed.nonce,
                &sealed.ciphertext,
            )
            .expect("open");
        assert_eq!(opened, b"xoxb-secret");
    }

    #[test]
    fn other_tenant_org_or_id_fails() {
        let dek = generate_key();
        let id = Uuid::new_v4();
        let sealed = row("t1", "acme", id)
            .seal(&dek, b"xoxb-secret")
            .expect("seal");
        let v = EncVersion::CURRENT.as_db();
        for other in [
            row("t2", "acme", id),
            row("t1", "other", id),
            row("t1", "acme", Uuid::new_v4()),
        ] {
            assert_eq!(
                other.open(&dek, v, &sealed.nonce, &sealed.ciphertext),
                Err(CryptoError::Decrypt),
                "{other:?} で復号できてはならない"
            );
        }
    }

    #[test]
    fn downgrade_to_legacy_fails() {
        // v1 の暗号文の enc_version を 0 へ書き換えても（AAD なしで開くことになり）復号できない。
        let dek = generate_key();
        let r = row("t1", "acme", Uuid::new_v4());
        let sealed = r.seal(&dek, b"xoxb-secret").expect("seal");
        assert_eq!(
            r.open(
                &dek,
                EncVersion::Legacy.as_db(),
                &sealed.nonce,
                &sealed.ciphertext
            ),
            Err(CryptoError::Decrypt)
        );
    }

    #[test]
    fn unknown_version_fails_closed() {
        let dek = generate_key();
        let r = row("t1", "acme", Uuid::new_v4());
        let sealed = r.seal(&dek, b"xoxb-secret").expect("seal");
        for v in [2, -1, i16::MAX] {
            assert_eq!(
                r.open(&dek, v, &sealed.nonce, &sealed.ciphertext),
                Err(CryptoError::Decrypt)
            );
        }
        assert_eq!(EncVersion::from_db(7), Err(CryptoError::Decrypt));
    }

    #[test]
    fn legacy_rows_still_open() {
        // #814 以前の行（AAD なし）は enc_version=0 として従来どおり復号できる。
        let dek = generate_key();
        let legacy = crypto::seal(&dek, b"old-secret", b"").expect("legacy seal");
        let r = row("t1", "acme", Uuid::new_v4());
        assert_eq!(
            r.open(
                &dek,
                EncVersion::Legacy.as_db(),
                &legacy.nonce,
                &legacy.ciphertext
            )
            .expect("legacy open"),
            b"old-secret"
        );
        // 旧版の暗号文を v1 として開くことはできない。
        assert_eq!(
            r.open(
                &dek,
                EncVersion::RowBound.as_db(),
                &legacy.nonce,
                &legacy.ciphertext
            ),
            Err(CryptoError::Decrypt)
        );
    }

    #[test]
    fn aad_is_unambiguous_across_field_boundaries() {
        let id = Uuid::new_v4();
        let a = row("a|b", "c", id).aad(EncVersion::RowBound);
        let b = row("a", "b|c", id).aad(EncVersion::RowBound);
        assert_ne!(a, b);
        assert!(row("a", "b", id).aad(EncVersion::Legacy).is_empty());
        assert_eq!(EncVersion::from_db(0), Ok(EncVersion::Legacy));
        assert_eq!(EncVersion::from_db(1), Ok(EncVersion::RowBound));
    }
}
