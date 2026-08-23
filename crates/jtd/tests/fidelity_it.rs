//! 忠実度ハーネス（トラックJTD）。
//!
//! 「きれいに写せているか」を主観にしないための計測。**以後のタスクのゲートにする。**
//!
//! # ここで見るもの（決定的・外部依存なし）
//!
//! 1. **公式テキストの回収率** — 厚生労働省が同じ様式で配布している text 版を
//!    独立オラクルとして、正規化したうえで「何割を順序どおり回収できたか」を測る。
//! 2. **ゴールデン一致** — 我々自身の出力の固定。改善も退行もここに出る。
//! 3. **docx パッケージの妥当性** — zip として開けて必要なパートが揃い、本文が入っていること。
//!
//! # ここで見ないもの
//!
//! 罫線位置・ページ割りの視覚比較は Collabora が要るので `scripts/jtd-fidelity.sh` が担う。
//! **Collabora が無い環境で黙ってスキップしない**ように、cargo test ではなく明示的な
//! スクリプトに分けてある（CLAUDE.md「意図したテストが実際に走ったかを確認する」）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Cursor, Read as _};

use jtd::JtdFile;

/// 公式テキストの回収率の下限。
///
/// 2026-08-23 の実測は **0.9815**。下回ったら本文の取りこぼしが増えたということ。
/// 1.0 にできないのは、公式 text 版が手作りの別版で、表のセルを 1 行に並べる等の
/// 版面差があるため（残差はほぼ表の並べ方。表の再構成は JTD.3）。
const MIN_REFERENCE_RECOVERY: f64 = 0.97;

fn fixture(name: &str) -> Vec<u8> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/");
    std::fs::read(format!("{path}{name}"))
        .unwrap_or_else(|error| panic!("フィクスチャ {name} が読めません: {error}"))
}

fn fixture_text(name: &str) -> String {
    String::from_utf8(fixture(name)).expect("フィクスチャは UTF-8")
}

/// 版面差を吸収する正規化。
///
/// 全角 ASCII を半角へ、全角スペースを空白へ寄せてから空白を落とす。
/// NFKC までは掛けない（依存を増やさず、実文書で効く差だけを潰す）。
fn normalize(text: &str) -> Vec<char> {
    text.chars()
        .map(|ch| match ch as u32 {
            code @ 0xff01..=0xff5e => char::from_u32(code - 0xfee0).unwrap_or(ch),
            0x3000 => ' ',
            _ => ch,
        })
        .filter(|ch| !ch.is_whitespace())
        .collect()
}

/// `reference` のうち `ours` に順序どおり現れる文字数の割合。
///
/// 最長共通部分列の長さ / 参照側の長さ。素朴な O(n·m) だが、実文書で 5,000 文字程度なので
/// これで足りる（実測 30ms 未満）。
fn recovery_ratio(reference: &[char], ours: &[char]) -> f64 {
    if reference.is_empty() {
        return 1.0;
    }
    let mut previous = vec![0usize; ours.len() + 1];
    let mut current = vec![0usize; ours.len() + 1];

    for reference_char in reference {
        for (index, ours_char) in ours.iter().enumerate() {
            current[index + 1] = if reference_char == ours_char {
                previous[index] + 1
            } else {
                current[index].max(previous[index + 1])
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }

    // 文字数は実文書で数千。f64 の仮数に収まる範囲なので精度の心配は無い。
    #[allow(clippy::cast_precision_loss)]
    let ratio = previous[ours.len()] as f64 / reference.len() as f64;
    ratio
}

/// docx の中の 1 パートを文字列で取り出す。
fn docx_part(docx: &[u8], path: &str) -> String {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(docx)).expect("docx は zip として開けること");
    let mut file = archive
        .by_name(path)
        .unwrap_or_else(|_| panic!("{path} が入っていること"));
    let mut out = String::new();
    file.read_to_string(&mut out).expect("UTF-8 であること");
    out
}

#[test]
fn recovers_the_official_text_rendition_of_f1() {
    let file = JtdFile::open(&fixture("f1.jtd")).expect("f1.jtd が読めること");

    let reference = normalize(&fixture_text("f1.reference.txt"));
    let ours = normalize(&file.plain_text());
    let ratio = recovery_ratio(&reference, &ours);

    assert!(
        ratio >= MIN_REFERENCE_RECOVERY,
        "公式テキストの回収率が {ratio:.4}（下限 {MIN_REFERENCE_RECOVERY}）。本文の取りこぼしが増えている"
    );
}

#[test]
fn f1_matches_its_golden_extraction() {
    let file = JtdFile::open(&fixture("f1.jtd")).expect("f1.jtd が読めること");

    assert_eq!(
        file.plain_text(),
        fixture_text("f1.golden.txt"),
        "抽出結果がゴールデンと違う。意図した改善なら fixtures/f1.golden.txt を更新する"
    );
}

#[test]
fn betu_matches_its_golden_extraction() {
    let file = JtdFile::open(&fixture("betu.jtd")).expect("betu.jtd が読めること");

    assert_eq!(
        file.plain_text(),
        fixture_text("betu.golden.txt"),
        "抽出結果がゴールデンと違う。意図した改善なら fixtures/betu.golden.txt を更新する"
    );
}

#[test]
fn keeps_the_first_line_that_precedes_every_record() {
    // 上流 `parse_document_text` は最初の `0x001F` を待つため、この行を落とす。
    // 公式 text 版の 1 行目と一致するので、これは本文である。
    let file = JtdFile::open(&fixture("f1.jtd")).expect("f1.jtd が読めること");

    assert!(
        file.plain_text().starts_with("様式第１（第７条関係）"),
        "最初のレコードより前にある本文が落ちている"
    );
}

#[test]
fn keeps_field_labels_stored_as_inline_segments() {
    // 申請書の欄見出しはインラインセグメントに入っている。捨てると本文から消える。
    let file = JtdFile::open(&fixture("f1.jtd")).expect("f1.jtd が読めること");
    let text = file.plain_text();

    for label in ["連絡先", "所属機関"] {
        assert!(text.contains(label), "欄見出し「{label}」が落ちている");
    }
}

#[test]
fn keeps_the_body_after_a_page_break() {
    // 0x000C（改ページ）を制御コードとして扱うと、直後の記入要領がまるごと落ちる。
    let file = JtdFile::open(&fixture("f1.jtd")).expect("f1.jtd が読めること");

    assert!(
        file.plain_text().contains("作成上の留意事項"),
        "改ページの後ろの本文が落ちている"
    );
}

#[test]
fn survives_a_false_positive_record_in_binary_regions() {
    // f1.jtd は埋め込み領域に「レコードに見えるが自己記述が整合しない」並びを 1 件持つ。
    // そこで打ち切ると後半 5,000 文字超が消える。文書末尾まで届いていることで確かめる。
    let file = JtdFile::open(&fixture("f1.jtd")).expect("f1.jtd が読めること");

    assert!(
        file.plain_text().contains("20．その他"),
        "偽陽性レコードで文書が途中まで消えている"
    );
}

#[test]
fn writes_a_valid_docx_package_for_each_fixture() {
    for name in ["f1.jtd", "betu.jtd"] {
        let file = JtdFile::open(&fixture(name)).unwrap_or_else(|error| panic!("{name}: {error}"));
        let docx = file
            .to_docx()
            .unwrap_or_else(|error| panic!("{name}: {error}"));

        assert_eq!(&docx[..2], b"PK", "{name}: zip で始まること");
        for part in [
            "[Content_Types].xml",
            "_rels/.rels",
            "word/document.xml",
            "word/_rels/document.xml.rels",
            "word/styles.xml",
        ] {
            let _ = docx_part(&docx, part);
        }

        let body = docx_part(&docx, "word/document.xml");
        let paragraphs = body.matches("<w:p>").count();
        assert_eq!(
            paragraphs,
            file.document().blocks().len(),
            "{name}: 段落数が中間モデルと一致すること"
        );
    }
}

#[test]
fn docx_preserves_the_ideographic_space_padding_of_the_form() {
    // 申請書の記入欄は全角スペースの連なりで升目を作っている。
    // `xml:space="preserve"` が落ちると原本の見た目が壊れる。
    let file = JtdFile::open(&fixture("f1.jtd")).expect("f1.jtd が読めること");
    let body = docx_part(
        &file.to_docx().expect("書き出せること"),
        "word/document.xml",
    );

    assert!(
        body.contains(r#"xml:space="preserve""#),
        "空白の保持指定が無い"
    );
    assert!(body.contains('　'), "全角スペースが docx に残っていない");
}

#[test]
fn recovery_ratio_is_a_meaningful_metric() {
    // 指標自体が壊れていないことを確かめる（常に 1.0 を返すような実装だと退行を検出できない）。
    let same: Vec<char> = "あいうえお".chars().collect();
    let half: Vec<char> = "あいう".chars().collect();
    let none: Vec<char> = "かきくけこ".chars().collect();

    assert!((recovery_ratio(&same, &same) - 1.0).abs() < f64::EPSILON);
    assert!((recovery_ratio(&same, &none) - 0.0).abs() < f64::EPSILON);
    assert!((recovery_ratio(&same, &half) - 0.6).abs() < 1e-9);
}
