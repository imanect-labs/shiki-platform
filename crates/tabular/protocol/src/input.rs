//! アップロードされた Excel（.xlsx）の上限（CSV を集計の読み取りだけ・D-17）。
//!
//! 展開と読み取りは隔離ランナーの中で行う（PIT-39）。アップロードされた Excel は敵対的な入力として扱い、展開の前と
//! 読みながら次の上限を確かめる（上限はここに置き、ランナーと api が同じ値を使う）。
//!
//! - ファイルの大きさ（[`MAX_FILE_BYTES`]）・zip の中の数（[`MAX_ENTRIES`]）
//! - 展開した大きさ（1 つずつ [`MAX_ENTRY_UNPACKED_BYTES`]・合わせて [`MAX_UNPACKED_BYTES`]）。zip が書く大きさは
//!   信じず、実際に展開したバイト数で数える（zip 爆弾）。
//! - 読むシートの値の数（[`MAX_CELLS`]）・列の幅（[`MAX_COLUMNS`]）・値の文字の合計（[`MAX_TEXT_BYTES`]。同じ共有の
//!   文字を何度も指すシートで、メモリを使い切らない）
//! - 表の大きさ（値のある行の数 × 列の幅・[`MAX_GRID_CELLS`]）。値の数と幅がそれぞれ上限の内でも、右端に離れた
//!   値が 1 つあるだけで、行ごとに幅の分の表を作る（100 万行 × 1,000 列）。ランナーは値を疎のまま持つが、表に
//!   入れるときは全部のセルを書くので、積も抑える。
//!
//! ランナーのプロセスには、DuckDB の memory_limit に加えてアドレス空間の上限も付ける（読み取りは Rust 側で確保し、
//! memory_limit が効かない・[`crate::protocol::runner_address_space_bytes`]）。
//!
//! 数式は計算しない（Excel が保存したときの値を読む・値の無い数式のセルは空）。外部のブックへの参照はたどらず、
//! XML の外部の実体も読まない。
//!
//! api 側だけが使うもの（先頭のバイトで形式を決める・CSV の名前の確かめ・その文言）は `tabular::input` にある。

/// Excel のファイルの大きさの上限（50 MiB）。
pub const MAX_FILE_BYTES: u64 = 50 << 20;
/// zip の中のファイルの数の上限。
pub const MAX_ENTRIES: usize = 10_000;
/// zip の中のファイル 1 つを展開した大きさの上限（64 MiB）。
pub const MAX_ENTRY_UNPACKED_BYTES: u64 = 64 << 20;
/// zip の中のファイルを合わせて展開した大きさの上限（256 MiB）。
pub const MAX_UNPACKED_BYTES: u64 = 256 << 20;
/// 読むシートの値（空でないセル）の数の上限。
pub const MAX_CELLS: usize = 1_000_000;
/// 読むシートの列の幅（左端の値から右端の値まで）の上限。
pub const MAX_COLUMNS: u32 = 1_000;
/// 読むシートの値の文字を合わせた大きさの上限（64 MiB）。
pub const MAX_TEXT_BYTES: usize = 64 << 20;
/// 読むシートの表の大きさ（値のある行の数 × 列の幅）の上限。
pub const MAX_GRID_CELLS: usize = 10_000_000;

/// Excel のファイルが大きすぎるとき。
#[must_use]
pub fn file_too_large() -> String {
    format!(
        "Excel のファイルが大きすぎます（{} MB まで）。使う行と列だけを CSV に保存して選んでください。",
        MAX_FILE_BYTES >> 20
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_too_large_says_the_limit_in_mb() {
        assert!(
            file_too_large().contains("50 MB まで"),
            "{}",
            file_too_large()
        );
    }
}
