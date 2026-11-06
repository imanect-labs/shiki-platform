//! 集計に渡すファイルの形式と、Excel（.xlsx）の受け方（CSV を集計の読み取りだけ・D-17）。
//!
//! api は先頭のバイトで形式を決めるだけで、Excel の中身（zip・XML）は解釈しない。展開と読み取りは隔離ランナーの中で
//! 行う（PIT-39）。アップロードされた Excel の上限（ファイルの大きさ・zip の中の数・展開した大きさ・値の数・列の幅・
//! 値の文字・表の大きさ）は、ランナーと共有するクレートの [`tabular_protocol::input`] に置き、ここで再エクスポートする
//! （ランナーと api が同じ値を使う・GAP-95）。ここに残すのは api の側だけが使うもの（先頭のバイトで形式を決める・
//! CSV の名前の確かめ・その文言）。

pub use tabular_protocol::input::{
    file_too_large, MAX_CELLS, MAX_COLUMNS, MAX_ENTRIES, MAX_ENTRY_UNPACKED_BYTES, MAX_FILE_BYTES,
    MAX_GRID_CELLS, MAX_TEXT_BYTES, MAX_UNPACKED_BYTES,
};

/// zip の先頭（ローカルファイルヘッダ）。.xlsx は zip。
const ZIP_MAGIC: &[u8] = b"PK\x03\x04";

/// OLE2 の複合ファイルの先頭。古い形式の Office（.xls など）と、パスワードを付けた Office のファイル。
const OLE_MAGIC: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// 先頭のバイトで決めた形式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sniffed {
    /// 文章（CSV として文字コードを決めて読む）。
    Text,
    /// zip（Excel のブックとしてランナーが読む。ブックでなければランナーが断る）。
    Zip,
    /// 古い形式の Office・パスワードを付けた Office（読まない）。
    OldOffice,
}

/// 先頭のバイトで形式を決める（中身は解釈しない）。
#[must_use]
pub fn sniff(bytes: &[u8]) -> Sniffed {
    if bytes.starts_with(ZIP_MAGIC) {
        Sniffed::Zip
    } else if bytes.starts_with(OLE_MAGIC) {
        Sniffed::OldOffice
    } else {
        Sniffed::Text
    }
}

/// 古い形式の Office・パスワードを付けたファイルを集計に渡したとき。
pub const OLD_OFFICE: &str = "古い形式の Excel（.xls）と、パスワードを付けたファイルは読めません。Excel で .xlsx か CSV に保存し直してください。";

/// CSV の編集に Excel などを渡したとき（編集は CSV だけ・D-17）。
pub const NOT_EDITABLE: &str =
    "編集できるのは CSV のファイルだけです。Excel（.xlsx）などのファイルは編集できません。";

/// 新しい CSV の名前が Excel の拡張子で終わるときの文言（保存は CSV だけで、Excel のファイルは作らない・D-17）。
///
/// ワークフローの「CSV を保存」は名前に `.csv` を足してから渡すので、末尾の `.csv` を 1 つ外した名前で見る
/// （「売上.xlsx」を「売上.xlsx.csv」として黙って作らない）。
#[must_use]
pub fn not_csv_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    // 拡張子は ASCII なので、末尾のバイトを ASCII の大小を無視して比べ、切る位置は元の名前から決める。
    // 名前全体を to_lowercase すると長さが変わる文字（「İ」・ケルビン記号など）で位置がずれる。
    let base = strip_ascii_suffix(trimmed, ".csv").unwrap_or(trimmed);
    let ext = [".xlsx", ".xlsm", ".xls"]
        .into_iter()
        .find(|ext| strip_ascii_suffix(base, ext).is_some())?;
    Some(format!(
        "「{base}」は Excel のファイルの名前（{ext}）です。保存できるのは CSV のファイルだけなので、名前の {ext} を外すか .csv にしてください。"
    ))
}

/// 末尾の ASCII の `suffix` を、大小を無視して外した残り。末尾が一致しなければ `None`。
fn strip_ascii_suffix<'a>(s: &'a str, suffix: &str) -> Option<&'a str> {
    let cut = s.len().checked_sub(suffix.len())?;
    // 一致した末尾は ASCII なので、cut は文字の境目になる。境目でなければ一致しない（ASCII と比べられない）。
    if !s.is_char_boundary(cut) {
        return None;
    }
    let (base, tail) = s.split_at(cut);
    tail.eq_ignore_ascii_case(suffix).then_some(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_by_the_first_bytes_only() {
        assert_eq!(sniff(b"PK\x03\x04rest"), Sniffed::Zip);
        assert_eq!(
            sniff(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0]),
            Sniffed::OldOffice
        );
        // 「PK」で始まる CSV は文章のまま（4 バイトの印がそろったときだけ zip）。
        assert_eq!(sniff(b"PK,amount\n1,2\n"), Sniffed::Text);
        assert_eq!(sniff(b""), Sniffed::Text);
    }

    #[test]
    fn refuses_excel_names_for_new_csv() {
        let m = not_csv_name("売上.XLSX").unwrap();
        assert!(
            m.contains("「売上.XLSX」") && m.contains("名前の .xlsx を外すか .csv にして"),
            "{m}"
        );
        // ワークフローは .csv を足してから渡す。
        let m = not_csv_name("売上.xlsx.csv").unwrap();
        assert!(m.contains("「売上.xlsx」"), "{m}");
        assert!(not_csv_name("売上.xls").is_some());
        assert!(not_csv_name("売上.xlsm ").is_some());
        assert!(not_csv_name("売上.csv").is_none());
        assert!(not_csv_name("売上").is_none());
        assert!(not_csv_name("xlsx の一覧.csv").is_none());
    }

    #[test]
    fn names_whose_lowercase_changes_length_do_not_panic() {
        // 「İ」（2 バイト）は小文字で 3 バイトに、ケルビン記号「K」（3 バイト）は 1 バイトになる。
        let m = not_csv_name("İ.xlsx").unwrap();
        assert!(m.contains("「İ.xlsx」"), "{m}");
        let m = not_csv_name("İİİİİ.xlsx.csv").unwrap();
        assert!(m.contains("「İİİİİ.xlsx」"), "{m}");
        let m = not_csv_name("\u{212A}\u{212A}.XLS").unwrap();
        assert!(m.contains("「\u{212A}\u{212A}.XLS」"), "{m}");
        assert!(not_csv_name("İİİİİ.csv").is_none());
        assert!(not_csv_name("\u{212A}\u{212A}\u{212A}\u{212A}.csv").is_none());
        // 末尾が多バイトの文字でも、拡張子と比べる位置が文字の途中にならない。
        assert!(not_csv_name("売").is_none());
        assert!(not_csv_name("あいう").is_none());
    }
}
