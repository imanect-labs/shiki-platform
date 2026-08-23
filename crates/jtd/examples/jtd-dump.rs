//! 一太郎ファイルを読んで中身を覗く開発用ツール（トラックJTD）。
//!
//! ```bash
//! cargo run -p shiki-jtd --example jtd-dump -- <file.jtd>          # 素性 ＋ 本文
//! cargo run -p shiki-jtd --example jtd-dump -- --streams <file.jtd> # CFB のストリーム一覧
//! cargo run -p shiki-jtd --example jtd-dump -- --hex <file.jtd>     # 本文ストリームの生バイト
//! cargo run -p shiki-jtd --example jtd-dump -- --docx out.docx <file.jtd>  # docx へ書き出す
//! cargo run -p shiki-jtd --example jtd-dump -- --text out.txt <file.jtd>   # ゴールデン更新用
//! ```
//!
//! `--hex` は `0x001C`〜`0x001F` の制御レコードを `<1C>` の形で見せる。表・罫線の解読は
//! ここを読むところから始まる（`vendor/openjtd/openjtd-spec/rfc/0003` と `0009`）。
//! より細かいプローブは所有フォーク側の CLI にある:
//! `cd vendor/openjtd/rjtd && cargo run -p rjtd-cli -- table-candidates <file.jtd>`

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::fmt::Write as _;
use std::process::ExitCode;

use jtd::JtdFile;

fn main() -> ExitCode {
    let mut streams = false;
    let mut hex = false;
    let mut docx: Option<String> = None;
    let mut text_out: Option<String> = None;
    let mut paths = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--streams" => streams = true,
            "--hex" => hex = true,
            "--docx" => docx = args.next(),
            "--text" => text_out = args.next(),
            _ => paths.push(arg),
        }
    }

    if paths.is_empty() {
        eprintln!("usage: jtd-dump [--streams] [--hex] <file.jtd>...");
        return ExitCode::FAILURE;
    }

    let mut failed = false;
    for path in &paths {
        if paths.len() > 1 {
            println!("===== {path}");
        }
        match dump(path, streams, hex, docx.as_deref(), text_out.as_deref()) {
            Ok(()) => {}
            Err(message) => {
                eprintln!("{path}: {message}");
                failed = true;
            }
        }
    }

    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn dump(
    path: &str,
    streams: bool,
    hex: bool,
    docx: Option<&str>,
    text_out: Option<&str>,
) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|error| format!("読めません: {error}"))?;
    let file = JtdFile::open(&bytes).map_err(|error| error.to_string())?;

    println!("format      : {}", file.format().as_str());
    println!("input       : {} bytes", bytes.len());
    match file.document_text_bytes() {
        Some(raw) => println!("DocumentText: {} bytes", raw.len()),
        None => println!(
            "fragments   : {} bytes（埋め込み断片・単一ストリームではない）",
            file.embedded_fragments().map_or(0, <[u8]>::len)
        ),
    }
    let text = file.plain_text();
    println!("text        : {} chars", text.chars().count());
    println!("paragraphs  : {}", file.document().blocks().len());

    if let Some(out) = text_out {
        // ゴールデン用。`plain_text()` の戻り値をそのまま書く（末尾改行を足さない）。
        std::fs::write(out, file.plain_text().as_bytes())
            .map_err(|error| format!("{out} に書けません: {error}"))?;
        println!("text        : {out}");
        return Ok(());
    }

    if let Some(out) = docx {
        let bytes = file.to_docx().map_err(|error| error.to_string())?;
        std::fs::write(out, &bytes).map_err(|error| format!("{out} に書けません: {error}"))?;
        println!("docx        : {out} ({} bytes)", bytes.len());
        return Ok(());
    }

    if streams {
        println!("--- streams ---");
        for stream in file.streams() {
            println!("{:>10}  {}", stream.size(), stream.path());
        }
    }

    if hex {
        println!("--- document text (control records as <XX>) ---");
        let raw = file
            .document_text_bytes()
            .or_else(|| file.embedded_fragments())
            .unwrap_or_default();
        println!("{}", annotate(raw));
    } else {
        println!("--- text ---");
        println!("{text}");
    }

    Ok(())
}

/// UTF-16BE として読み、制御コード（`0x20` 未満）を `<XX>` で見せる。
fn annotate(raw: &[u8]) -> String {
    // 先頭 8 バイトは `SsmgV.01` のマジックで、UTF-16 の単位ではない。
    let body = raw.get(8..).unwrap_or_default();
    let mut out = String::new();
    for chunk in body.chunks_exact(2) {
        let unit = u16::from_be_bytes([chunk[0], chunk[1]]);
        match unit {
            0x000a => out.push('\n'),
            0x0009 => out.push('\t'),
            0x0000..=0x001f => {
                let _ = write!(out, "<{unit:02X}>");
            }
            _ => out.push(char::from_u32(u32::from(unit)).unwrap_or('\u{fffd}')),
        }
    }
    out
}
