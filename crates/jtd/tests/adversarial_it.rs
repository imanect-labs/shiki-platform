//! 細工した CFB ヘッダに対する耐性（トラックJTD）。
//!
//! JTD は外部由来のバイナリで、パースは worker 往復ではなく shiki-server の**プロセス内**で走る。
//! よって「不正な入力でエラーを返す」だけでは足りず、**有界な時間とメモリで**返る必要がある。
//! ここで押さえるのは、独立レビューで実際に見つかった 2 件の退行:
//!
//! - 1 KiB のファイルがヘッダで `difat_sector_count = 0xFFFFFFFF` を申告すると、上流の
//!   lenient CFB リーダが自己参照する DIFAT セクタを延々と辿り、`sector_ids` が 2 GiB を超えて
//!   **確保失敗でプロセスが abort** していた。abort は unwind ではないため `catch_unwind` では
//!   捕まえられない（＝ API 全体が落ちる）。
//! - セクタシフト 0（`sector_size = 1`）で `sector_size / 4 - 1` が underflow し、
//!   debug ではパニック、release では `usize::MAX` 回のループになっていた。
//!
//! 修正は `vendor/openjtd/patches/0001-bound-difat-walk.patch`。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use jtd::{JtdError, JtdFile};

/// 細工した入力 1 件あたりの許容時間。実測は μs オーダーなので、
/// これに触れたら「有界でなくなった」と判断してよい。
const BUDGET: Duration = Duration::from_secs(5);

const CFB_MAGIC: &[u8; 8] = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1";

fn put16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// CFB ヘッダだけを手で組む（`cfb` クレートでは作れない不正な組み合わせを作るため）。
fn crafted_header(len: usize, sector_shift: u16) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    bytes[..8].copy_from_slice(CFB_MAGIC);
    put16(&mut bytes, 30, sector_shift);
    put32(&mut bytes, 48, 0xffff_fffe); // first_directory_sector = ENDOFCHAIN
    put32(&mut bytes, 56, 4096); // mini_stream_cutoff
    put32(&mut bytes, 60, 0xffff_fffe); // first_mini_fat_sector = ENDOFCHAIN
    bytes
}

/// 経過時間つきで開く。
fn open_timed(bytes: &[u8]) -> (Result<JtdFile, JtdError>, Duration) {
    let started = Instant::now();
    let result = JtdFile::open(bytes);
    (result, started.elapsed())
}

#[test]
fn self_referencing_difat_chain_does_not_exhaust_memory() {
    // 自分自身を次に指す DIFAT セクタ ＋ 巨大な difat_sector_count。
    // 修正前はここで `sector_ids` が 2 GiB を超え、確保失敗で abort していた。
    let mut bytes = crafted_header(1024, 9); // sector_size = 512
    put32(&mut bytes, 44, 1); // fat_sector_count
    put32(&mut bytes, 68, 0); // first_difat_sector = 0（データ内の実セクタ）
    put32(&mut bytes, 72, 0xffff_ffff); // difat_sector_count = u32::MAX
    for index in 0..109 {
        put32(&mut bytes, 76 + index * 4, 0xffff_ffff); // ヘッダ DIFAT は空
    }
    // セクタ 0 の中身: 有効なセクタ id を埋め、末尾の next も自分（= 0）を指す。
    for index in 0..128 {
        put32(&mut bytes, 512 + index * 4, 0);
    }

    let (result, elapsed) = open_timed(&bytes);

    assert!(result.is_err(), "細工した入力は拒否されること");
    assert!(elapsed < BUDGET, "有界な時間で返ること（実測 {elapsed:?}）");
}

#[test]
fn sector_size_below_four_does_not_hang() {
    // セクタシフト 0 → sector_size = 1。修正前は `sector_size / 4 - 1` が underflow していた。
    let mut bytes = crafted_header(512, 0);
    put32(&mut bytes, 68, 0); // first_difat_sector = 0
    put32(&mut bytes, 72, 1); // difat_sector_count = 1

    let (result, elapsed) = open_timed(&bytes);

    assert!(result.is_err(), "細工した入力は拒否されること");
    assert!(elapsed < BUDGET, "有界な時間で返ること（実測 {elapsed:?}）");
}

#[test]
fn oversized_declared_counts_are_bounded_by_input_size() {
    // fat / difat の申告値だけを膨らませたもの。入力が持ちうるセクタ数で頭打ちになる。
    for declared in [10_000u32, 1_000_000, u32::MAX] {
        let mut bytes = crafted_header(1024, 9);
        put32(&mut bytes, 44, declared); // fat_sector_count
        put32(&mut bytes, 68, 0);
        put32(&mut bytes, 72, declared); // difat_sector_count
        for index in 0..127 {
            put32(&mut bytes, 512 + index * 4, 5);
        }
        put32(&mut bytes, 512 + 127 * 4, 0); // next = 自分

        let (result, elapsed) = open_timed(&bytes);

        assert!(result.is_err(), "declared={declared} は拒否されること");
        assert!(
            elapsed < BUDGET,
            "declared={declared} でも有界な時間で返ること（実測 {elapsed:?}）"
        );
    }
}

#[test]
fn header_without_body_is_rejected() {
    // CFB マジックだけがあり中身が無いファイル。
    let bytes = crafted_header(512, 9);

    let (result, elapsed) = open_timed(&bytes);

    assert!(
        matches!(result, Err(JtdError::Unsupported)),
        "一太郎として解釈できない複合文書は Unsupported"
    );
    assert!(elapsed < BUDGET, "有界な時間で返ること（実測 {elapsed:?}）");
}

#[test]
fn truncated_input_is_rejected() {
    // CFB を名乗るには 512 バイト必要。それに満たない入力。
    let bytes = &CFB_MAGIC[..];

    let (result, _) = open_timed(bytes);

    assert!(
        matches!(result, Err(JtdError::Unsupported)),
        "切り詰められた入力も落ちずに拒否されること"
    );
}
