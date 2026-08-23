//! 細工した CFB ヘッダに対する耐性（トラックJTD）。
//!
//! JTD は外部由来のバイナリで、パースは worker 往復ではなく shiki-server の**プロセス内**で走る。
//! よって「不正な入力でエラーを返す」だけでは足りず、**有界な時間とメモリで**返る必要がある。
//! ここで押さえるのは、独立レビューで実際に踏み抜いた退行:
//!
//! - **`0001`** 1 KiB のファイルがヘッダで `difat_sector_count = 0xFFFFFFFF` を申告すると、
//!   上流の lenient CFB リーダが自己参照する DIFAT セクタを延々と辿り、`sector_ids` が 2 GiB を
//!   超えて**確保失敗でプロセスが abort** していた。abort は unwind ではないため
//!   `catch_unwind` では捕まえられない（＝ API 全体が落ちる）。
//!   併せてセクタシフト 0（`sector_size = 1`）で `sector_size / 4 - 1` が underflow していた。
//! - **`0002`** ディレクトリエントリの `left_id` を数珠つなぎにすると、上流の
//!   `assign_child_tree_paths` が再帰でそれを辿り、約 1 MiB のファイルで
//!   **スタックオーバーフロー → abort** した。
//! - **`0003`** `SsmgV.01` の断片を敷き詰めると、埋め込みテキストの重複排除が **O(N²)** で
//!   回った（1 MiB で 29 秒、2 MiB で 133 秒）。
//!
//! 修正は `vendor/openjtd/patches/000{1,2,3}-*.patch`。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Cursor, Write};
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

/// 128 バイトのディレクトリエントリを組む。
fn directory_entry(name: &str, object_type: u8, left: u32, right: u32, child: u32) -> Vec<u8> {
    let mut entry = vec![0u8; 128];
    let units: Vec<u16> = name.encode_utf16().collect();
    for (index, unit) in units.iter().enumerate() {
        entry[index * 2..index * 2 + 2].copy_from_slice(&unit.to_le_bytes());
    }
    // 名前長は終端 NUL を含むバイト数。
    put16(&mut entry, 64, (units.len() as u16 + 1) * 2);
    entry[66] = object_type;
    put32(&mut entry, 68, left);
    put32(&mut entry, 72, right);
    put32(&mut entry, 76, child);
    entry
}

#[test]
fn deep_directory_spine_does_not_overflow_the_stack() {
    // ディレクトリエントリの left_id を数珠つなぎにした CFB。修正前は上流の
    // `assign_child_tree_paths` が再帰でこれを辿り、**スタックオーバーフローで
    // プロセスが abort** した（abort は unwind ではないので catch_unwind では捕まらない）。
    //
    // 退行するとこのテストは assert 失敗ではなくテストバイナリのクラッシュとして出る。
    // それでも CI は赤くなるので検出器としては成立する。
    const SECTOR_SIZE: usize = 4096;
    const ENTRIES_PER_SECTOR: usize = SECTOR_SIZE / 128;
    const DIR_SECTORS: usize = 256;
    const ENTRY_COUNT: usize = DIR_SECTORS * ENTRIES_PER_SECTOR; // 8192 段の左スパイン
    const NONE: u32 = 0xffff_ffff;
    const END_OF_CHAIN: u32 = 0xffff_fffe;

    let total_sectors = 1 + DIR_SECTORS; // FAT 1 本 ＋ ディレクトリ
    let mut bytes = vec![0u8; SECTOR_SIZE * (1 + total_sectors)];
    bytes[..8].copy_from_slice(CFB_MAGIC);
    put16(&mut bytes, 30, 12); // sector_shift → sector_size = 4096
    put32(&mut bytes, 44, 1); // fat_sector_count
    put32(&mut bytes, 48, 1); // first_directory_sector
    put32(&mut bytes, 56, 4096); // mini_stream_cutoff
    put32(&mut bytes, 60, END_OF_CHAIN); // first_mini_fat_sector
    put32(&mut bytes, 68, END_OF_CHAIN); // first_difat_sector
    put32(&mut bytes, 76, 0); // ヘッダ DIFAT[0] = FAT はセクタ 0

    // FAT（セクタ 0）: セクタ 0 は FAT 自身、1..DIR_SECTORS はディレクトリの鎖。
    let fat = SECTOR_SIZE;
    put32(&mut bytes, fat, END_OF_CHAIN);
    for sector in 1..=DIR_SECTORS {
        let next = if sector == DIR_SECTORS {
            END_OF_CHAIN
        } else {
            sector as u32 + 1
        };
        put32(&mut bytes, fat + sector * 4, next);
    }
    for slot in (DIR_SECTORS + 1)..(SECTOR_SIZE / 4) {
        put32(&mut bytes, fat + slot * 4, NONE);
    }

    // ディレクトリ（セクタ 1〜）: 0 番が Root、以降は left_id で next を指す一本鎖。
    let dir = SECTOR_SIZE * 2;
    let mut entries = directory_entry("Root Entry", 5, NONE, NONE, 1);
    for index in 1..ENTRY_COUNT {
        let left = if index + 1 < ENTRY_COUNT {
            index as u32 + 1
        } else {
            NONE
        };
        entries.extend_from_slice(&directory_entry("s", 2, left, NONE, NONE));
    }
    bytes[dir..dir + entries.len()].copy_from_slice(&entries);

    let (result, elapsed) = open_timed(&bytes);

    assert!(
        result.is_err(),
        "一太郎の本文を持たない複合文書は拒否されること"
    );
    assert!(elapsed < BUDGET, "有界な時間で返ること（実測 {elapsed:?}）");
}

#[test]
fn header_without_body_is_rejected() {
    // CFB マジックだけがあり中身が無いファイル。
    let bytes = crafted_header(512, 9);

    let (result, elapsed) = open_timed(&bytes);

    assert!(
        matches!(result, Err(JtdError::Malformed)),
        "ディレクトリに到達できない CFB は「壊れている」に分類されること"
    );
    assert!(elapsed < BUDGET, "有界な時間で返ること（実測 {elapsed:?}）");
}

#[test]
fn truncated_input_is_rejected() {
    // CFB を名乗るには 512 バイト必要。マジックだけあって本体が無い入力は
    // 「別形式」ではなく「壊れた CFB」なので Malformed に落ちる。
    let bytes = &CFB_MAGIC[..];

    let (result, _) = open_timed(bytes);

    assert!(
        matches!(result, Err(JtdError::Malformed)),
        "切り詰められた入力も落ちずに拒否されること"
    );
}

#[test]
fn many_embedded_text_fragments_stay_linear() {
    // `SsmgV.01` は入力のどこに何個あってもよく、18 バイトあれば 1 断片になる。
    // 修正前は断片ごとに既出テキスト全体と線形比較していたため O(N^2) で、
    // 1 MiB のファイルが 29 秒、既定上限では時間単位に達した。
    // ここは `/DocumentText` も `/JSCompDocument` も持たない CFB なので、
    // 埋め込み断片の走査（`has_embedded_document_text`）に落ちる。
    const FRAGMENT_COUNT: usize = 32_768; // 断片 16B ＋ ヘッダで約 512 KiB

    let mut payload = Vec::with_capacity(FRAGMENT_COUNT * 16);
    for index in 0..FRAGMENT_COUNT {
        payload.extend_from_slice(b"SsmgV.01");
        payload.extend_from_slice(&0x001fu16.to_be_bytes());
        // 断片ごとに異なるテキストにして、重複排除を最悪ケースで踏ませる。
        for unit in format!("{index:04x}").encode_utf16().take(3) {
            payload.extend_from_slice(&unit.to_be_bytes());
        }
    }

    let mut compound = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
    let mut stream = compound.create_stream("/WordDocument").unwrap();
    stream.write_all(&payload).unwrap();
    drop(stream);
    let bytes = compound.into_inner().into_inner();

    let (_, elapsed) = open_timed(&bytes);

    assert!(
        elapsed < BUDGET,
        "断片数に対して線形であること（実測 {elapsed:?}）"
    );
}
