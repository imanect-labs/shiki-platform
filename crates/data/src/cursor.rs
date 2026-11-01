//! keyset カーソルの AEAD コーデック（Phase 15 Task 15.1・data-platform §3.2 / §5.3・PIT-75）。
//!
//! カーソルは「全ソートキーの値 ＋ `record_int`（＋任意列ソートの区間番号）」を持つ。`record_int` は
//! プール共通の連番で、外へ出すと他テナントの作成ペースを推測させる（PIT-75）。base64 だけでは
//! 中身が読めるので、**サーバ鍵の AEAD（AES-256-GCM）で暗号化した不透明トークン**にする。
//!
//! # 束縛（流用の拒否）
//!
//! 付随データ（AAD）に `tenant_int` / `table_int` と呼び出し側の束縛バイト列（ソート指定の
//! 指紋など）を入れる。別テナント・別テーブル・別ソートへ持ち込まれたカーソルは**認証タグの
//! 検証で失敗**し、改竄も同様に失敗する。失敗理由は区別せず同一の文言で返す（オラクルを作らない）。
//!
//! # 形式
//!
//! `base64url(nopad)( version(1) || nonce(12) || ciphertext+tag )`。平文は JSON
//! （[`CursorPayload`]）。nonce は毎回乱数（96bit・GCM の要件）。
//!
//! クエリコンパイラ v2（[`crate::query`]）の keyset ページングが使う。

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine as _;
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::DataError;

/// 鍵長（AES-256）。
pub const CURSOR_KEY_LEN: usize = 32;
/// トークン形式のバージョン（鍵ローテーション・形式変更時に上げる）。
const VERSION: u8 = 1;
const NONCE_LEN: usize = 12;
/// 受理するトークン長の上限（復号前に巨大入力を弾く）。
const MAX_TOKEN_LEN: usize = 4_096;
/// AAD のドメイン分離ラベル（他用途の暗号文と取り違えない）。
const AAD_LABEL: &[u8] = b"shiki/data/cursor/v1";
/// テーブル一覧の続きの位置（[`CursorCodec::seal_table_list`]）の AAD のラベル（レコードのカーソルと取り違えない）。
const TABLE_LIST_AAD_LABEL: &[u8] = b"shiki/data/table-list-cursor/v1";
/// ほかの一覧の続きの位置（[`CursorCodec::seal_list_position`]）の AAD のラベル。
const LIST_POSITION_AAD_LABEL: &[u8] = b"shiki/list-position/v1";

/// カーソル暗号鍵（32 バイト）。鍵ファイルの形式は secrets のマスターキーと同じ。
#[derive(Clone)]
pub struct CursorKey([u8; CURSOR_KEY_LEN]);

impl CursorKey {
    pub fn from_bytes(bytes: [u8; CURSOR_KEY_LEN]) -> Self {
        CursorKey(bytes)
    }

    /// マスターキーから用途別の鍵を導出する（HMAC-SHA256・ドメイン分離ラベル付き）。
    /// 同じマスターキーを持つ全レプリカで同じ鍵になる。マスターキー自体を AEAD の鍵に流用しない。
    pub fn derive_from_master(master: &[u8; CURSOR_KEY_LEN]) -> Self {
        use hmac::{Hmac, Mac};
        // HMAC は任意長の鍵を受けるので new_from_slice は失敗しない（失敗時は乱数鍵へ倒さず空鍵で導出しない）。
        let mut k = [0u8; CURSOR_KEY_LEN];
        if let Ok(mut mac) = <Hmac<sha2::Sha256> as Mac>::new_from_slice(master) {
            mac.update(b"shiki/data/cursor-key/v1");
            k.copy_from_slice(&mac.finalize().into_bytes());
        }
        CursorKey(k)
    }

    /// 起動ごとの乱数鍵（ライブラリの既定・開発/テスト用。再起動・レプリカ間でカーソルは無効になる）。
    pub fn ephemeral() -> Self {
        let mut k = [0u8; CURSOR_KEY_LEN];
        rand::thread_rng().fill_bytes(&mut k);
        CursorKey(k)
    }
}

impl std::fmt::Debug for CursorKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CursorKey(<redacted>)")
    }
}

/// カーソルの中身（**crate 外へ出さない**。外へ出るのは [`CursorCodec::seal`] の暗号文だけ）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct CursorPayload {
    /// 全ソートキーの値（ソート指定の順）。
    pub sort_values: Vec<serde_json::Value>,
    /// 継続位置の内部キー。
    pub record_int: i64,
    /// 任意列ソートの区間番号（1 = 値を持つ行・2 = 値なしの行・data-platform §3.5）。
    pub segment: u8,
    /// スナップショット読取の基準位置（data-platform §3.3）。配信位置（`delivery_seq`）は
    /// Task 15.6 の outbox で入るため、現状は常に `None`（ライブページング）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_seq: Option<i64>,
    /// 結合を残した行のページング（FROM の行の途中で切れた組の続き・Task 15.10）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join: Option<JoinCursor>,
}

/// 結合を残した行の継続位置（FROM の行 1 つが複数の組に広がるため、行の途中から続ける）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct JoinCursor {
    /// `sort_values` / `record_int` が「続きの行の直前の位置」を持つか（先頭の行の途中なら false）。
    pub has_base: bool,
    /// 途中まで返した FROM の行。
    pub rint: i64,
    /// その行について既に返した組の数。
    pub skip: u32,
}

/// カーソルの束縛先（AAD に入る・トークンには含まれない）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct CursorScope<'a> {
    pub tenant_int: i32,
    pub table_int: i64,
    /// 呼び出し側の追加束縛（ソート指定・フィルタの指紋など）。
    pub binding: &'a [u8],
}

impl CursorScope<'_> {
    fn aad(&self) -> Vec<u8> {
        let mut aad = Vec::with_capacity(AAD_LABEL.len() + 1 + 4 + 8 + self.binding.len());
        aad.extend_from_slice(AAD_LABEL);
        aad.push(VERSION);
        aad.extend_from_slice(&self.tenant_int.to_be_bytes());
        aad.extend_from_slice(&self.table_int.to_be_bytes());
        aad.extend_from_slice(self.binding);
        aad
    }
}

/// AEAD コーデック。
pub(crate) struct CursorCodec {
    cipher: Aes256Gcm,
}

impl CursorCodec {
    pub(crate) fn new(key: &CursorKey) -> Self {
        CursorCodec {
            cipher: Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key.0)),
        }
    }

    /// 暗号化して不透明トークンにする。
    pub(crate) fn seal(
        &self,
        scope: &CursorScope<'_>,
        payload: &CursorPayload,
    ) -> Result<String, DataError> {
        let plain = serde_json::to_vec(payload)
            .map_err(|e| DataError::Internal(format!("cursor encode: {e}")))?;
        self.seal_bytes(&scope.aad(), &plain)
    }

    /// トークンを検証・復号する。改竄・別スコープへの流用・形式不正はすべて同一の 422。
    pub(crate) fn open(
        &self,
        scope: &CursorScope<'_>,
        token: &str,
    ) -> Result<CursorPayload, DataError> {
        let plain = self.open_bytes(&scope.aad(), token)?;
        serde_json::from_slice(&plain)
            .map_err(|_| DataError::input(crate::InvalidCode::CursorInvalid))
    }

    /// テーブル一覧の続きの位置を暗号化する（テナントに束縛・BUG-149）。
    ///
    /// 一覧は見えないテーブルの位置で止まることがある（DB の候補を OpenFGA で絞る経路の確かめる件数の
    /// 上限）。位置をそのまま出すと、見えないテーブルの id と更新の時刻（名前で探したときは名前に当たる
    /// ことも）が分かるので、レコードのカーソルと同じ鍵で暗号化する。
    pub(crate) fn seal_table_list(
        &self,
        tenant_id: &str,
        plain: &[u8],
    ) -> Result<String, DataError> {
        self.seal_bytes(&table_list_aad(tenant_id), plain)
    }

    /// [`Self::seal_table_list`] の逆。改竄・別テナントへの流用・形式不正は `cursor_invalid`。
    pub(crate) fn open_table_list(
        &self,
        tenant_id: &str,
        token: &str,
    ) -> Result<Vec<u8>, DataError> {
        self.open_bytes(&table_list_aad(tenant_id), token)
    }

    /// ほかの一覧の続きの位置を暗号化する（テナントと一覧の種類に束縛・BUG-162・[`crate::list_position`]）。
    pub(crate) fn seal_list_position(
        &self,
        kind: &[u8],
        tenant_id: &str,
        plain: &[u8],
    ) -> Result<String, DataError> {
        self.seal_bytes(&list_position_aad(kind, tenant_id), plain)
    }

    /// [`Self::seal_list_position`] の逆。改竄・別テナント・別の一覧への流用・形式不正は `cursor_invalid`。
    pub(crate) fn open_list_position(
        &self,
        kind: &[u8],
        tenant_id: &str,
        token: &str,
    ) -> Result<Vec<u8>, DataError> {
        self.open_bytes(&list_position_aad(kind, tenant_id), token)
    }

    fn seal_bytes(&self, aad: &[u8], plain: &[u8]) -> Result<String, DataError> {
        let mut nonce = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce);
        let ct = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: plain, aad })
            .map_err(|_| DataError::Internal("cursor seal".into()))?;
        let mut raw = Vec::with_capacity(1 + NONCE_LEN + ct.len());
        raw.push(VERSION);
        raw.extend_from_slice(&nonce);
        raw.extend_from_slice(&ct);
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw))
    }

    fn open_bytes(&self, aad: &[u8], token: &str) -> Result<Vec<u8>, DataError> {
        let invalid = || DataError::input(crate::InvalidCode::CursorInvalid);
        if token.is_empty() || token.len() > MAX_TOKEN_LEN {
            return Err(invalid());
        }
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(token)
            .map_err(|_| invalid())?;
        if raw.len() < 1 + NONCE_LEN + 16 || raw[0] != VERSION {
            return Err(invalid());
        }
        let (nonce, ct) = raw[1..].split_at(NONCE_LEN);
        self.cipher
            .decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad })
            .map_err(|_| invalid())
    }
}

/// テーブル一覧の続きの位置の AAD（ラベル・形式の版・テナント）。
fn table_list_aad(tenant_id: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(TABLE_LIST_AAD_LABEL.len() + 1 + tenant_id.len());
    aad.extend_from_slice(TABLE_LIST_AAD_LABEL);
    aad.push(VERSION);
    aad.extend_from_slice(tenant_id.as_bytes());
    aad
}

/// ほかの一覧の続きの位置の AAD（ラベル・版・一覧の種類・テナント。種類とテナントは長さで区切る）。
fn list_position_aad(kind: &[u8], tenant_id: &str) -> Vec<u8> {
    let mut aad =
        Vec::with_capacity(LIST_POSITION_AAD_LABEL.len() + 9 + kind.len() + tenant_id.len());
    aad.extend_from_slice(LIST_POSITION_AAD_LABEL);
    aad.push(VERSION);
    aad.extend_from_slice(&u32::try_from(kind.len()).unwrap_or(u32::MAX).to_be_bytes());
    aad.extend_from_slice(kind);
    aad.extend_from_slice(tenant_id.as_bytes());
    aad
}

#[cfg(test)]
#[path = "cursor_tests.rs"]
mod tests;
