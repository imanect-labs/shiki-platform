//! カーソル AEAD コーデックの単体テスト（改竄・別テーブル/別テナント/別束縛への流用の拒否）。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use serde_json::json;

/// 識別しやすい大きな record_int（トークンに平文で現れないことの検査用）。
const RECORD_INT: i64 = 7_314_159_265_358;

fn payload() -> CursorPayload {
    CursorPayload {
        sort_values: vec![json!("承認待ち"), json!(12_000)],
        record_int: RECORD_INT,
        segment: 1,
    }
}

fn scope(tenant_int: i32, table_int: i64) -> CursorScope<'static> {
    CursorScope {
        tenant_int,
        table_int,
        binding: b"sort:f16:desc",
    }
}

fn codec() -> CursorCodec {
    CursorCodec::new(&CursorKey::from_bytes([7u8; CURSOR_KEY_LEN]))
}

fn is_rejected(r: &Result<CursorPayload, DataError>) -> bool {
    matches!(r, Err(DataError::Invalid(m)) if m == INVALID)
}

#[test]
fn roundtrip_within_same_scope() {
    let c = codec();
    let token = c.seal(&scope(1, 100), &payload()).unwrap();
    assert_eq!(c.open(&scope(1, 100), &token).unwrap(), payload());
    // nonce は毎回変わる（同じ内容でもトークンが一致しない＝比較で中身を推測させない）。
    let again = c.seal(&scope(1, 100), &payload()).unwrap();
    assert_ne!(token, again);
}

#[test]
fn token_does_not_reveal_record_int() {
    let c = codec();
    let token = c.seal(&scope(1, 100), &payload()).unwrap();
    assert!(!token.contains(&RECORD_INT.to_string()), "{token}");
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&token)
        .unwrap();
    let as_text = String::from_utf8_lossy(&raw);
    assert!(!as_text.contains(&RECORD_INT.to_string()));
    assert!(!as_text.contains("record_int"));
}

#[test]
fn tampering_any_byte_is_rejected() {
    let c = codec();
    let token = c.seal(&scope(1, 100), &payload()).unwrap();
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&token)
        .unwrap();
    for i in 0..raw.len() {
        let mut t = raw.clone();
        t[i] ^= 0x01;
        let forged = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&t);
        assert!(
            is_rejected(&c.open(&scope(1, 100), &forged)),
            "byte {i} の改竄が通った"
        );
    }
    // 切り詰め・延長も拒否。
    let short = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&raw[..raw.len() - 1]);
    assert!(is_rejected(&c.open(&scope(1, 100), &short)));
    let mut long = raw.clone();
    long.push(0);
    let long = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&long);
    assert!(is_rejected(&c.open(&scope(1, 100), &long)));
}

#[test]
fn reuse_on_other_table_or_tenant_is_rejected() {
    let c = codec();
    let token = c.seal(&scope(1, 100), &payload()).unwrap();
    // 別テーブル（同一テナント）。
    assert!(is_rejected(&c.open(&scope(1, 101), &token)));
    // 別テナント（同じ table_int を名乗っても AAD が違う）。
    assert!(is_rejected(&c.open(&scope(2, 100), &token)));
    // 別のソート指定（束縛バイト列）。
    let other_sort = CursorScope {
        tenant_int: 1,
        table_int: 100,
        binding: b"sort:f17:asc",
    };
    assert!(is_rejected(&c.open(&other_sort, &token)));
}

#[test]
fn other_key_and_garbage_are_rejected() {
    let token = codec().seal(&scope(1, 100), &payload()).unwrap();
    let other = CursorCodec::new(&CursorKey::from_bytes([8u8; CURSOR_KEY_LEN]));
    assert!(is_rejected(&other.open(&scope(1, 100), &token)));
    let c = codec();
    for bad in ["", "!!!!", "AAAA", &"A".repeat(MAX_TOKEN_LEN + 1)] {
        assert!(is_rejected(&c.open(&scope(1, 100), bad)), "{bad:.16}");
    }
    // 平文 JSON を base64 しただけの偽造（AEAD を通らない）も拒否。
    let naive = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&payload()).unwrap());
    assert!(is_rejected(&c.open(&scope(1, 100), &naive)));
}

#[test]
fn key_debug_is_redacted() {
    let k = CursorKey::from_bytes([9u8; CURSOR_KEY_LEN]);
    assert_eq!(format!("{k:?}"), "CursorKey(<redacted>)");
}
