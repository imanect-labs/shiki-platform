//! `sdk/first-party-skills/` の配布バンドルが保存時検証を通ることを CI で固定する（#387）。
//!
//! バンドルは**署名 import 経路**（`POST /skills/registry/import`）で入るため、通常の保存 API の
//! バリデーションを人が踏まない。上限超過（instructions 32KB・description 1024 文字）や
//! `command` の形式違反は import 時まで気付けず、その時には署名をやり直すことになる。
//! ここでリポジトリ内のバンドル全件を [`gui::validate_skill_body`] に通しておく。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

/// `sdk/first-party-skills` の絶対パス（このクレートからの相対）。
fn bundles_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../sdk/first-party-skills")
        .canonicalize()
        .expect("sdk/first-party-skills が存在すること")
}

/// バンドル（`<name>/skill.json`）を列挙する。
fn bundles() -> Vec<(String, serde_json::Value)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(bundles_dir()).expect("ディレクトリ読み取り") {
        let entry = entry.expect("エントリ");
        let manifest = entry.path().join("skill.json");
        if !manifest.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let raw = std::fs::read_to_string(&manifest)
            .unwrap_or_else(|e| panic!("{name}/skill.json を読めない: {e}"));
        let body: serde_json::Value = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("{name}/skill.json が JSON として不正: {e}"));
        out.push((name, body));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn all_first_party_bundles_pass_validation() {
    let found = bundles();
    assert!(!found.is_empty(), "バンドルが 1 件以上あること");
    for (name, body) in found {
        let validated = gui::validate_skill_body(&body)
            .unwrap_or_else(|errors| panic!("{name}/skill.json が検証を通らない: {errors:?}"));
        assert!(
            !validated.instructions.trim().is_empty(),
            "{name}: instructions が空"
        );
        assert!(
            !validated.description.trim().is_empty(),
            "{name}: description が空"
        );
    }
}

/// deep-research は**コマンド起動が本体**なので、宣言の中身まで固定する（#387）。
///
/// `/deep-research` と `/deep-research auto` の 2 経路はスラッシュコマンド UI の
/// variant 一覧としてそのまま出る。ここが崩れると起動導線が消える。
#[test]
fn deep_research_declares_both_command_variants() {
    let (_, body) = bundles()
        .into_iter()
        .find(|(name, _)| name == "deep-research")
        .expect("deep-research バンドルがあること");
    let skill = gui::validate_skill_body(&body).expect("検証を通る");

    let command = skill.command.expect("command 宣言がある");
    assert_eq!(command.name, "deep-research");
    let args: Vec<&str> = command.variants.iter().map(|v| v.args.as_str()).collect();
    assert_eq!(
        args,
        vec!["", "auto"],
        "既定（質問→計画→実行）と auto（即実行）の 2 経路"
    );

    // 自律プロファイル前提のスキル: 調査・作業ファイル・UI・下書き保存が宣言に揃っていること
    // （`allowed_tools` は誘導テキストなので実行を縛らないが、宣言の欠落は instructions と
    // 齟齬を生む）。追記が抜けると証拠台帳が全文置換に退化する（#392）。
    let tools: Vec<&str> = skill
        .allowed_tools
        .as_ref()
        .expect("allowed_tools を宣言する")
        .iter()
        .map(|t| t.as_str())
        .collect();
    for expected in [
        "web_search",
        "web_fetch",
        "doc_search",
        "emit_ui",
        "fs_append",
        "save_note",
    ] {
        assert!(tools.contains(&expected), "{expected} が宣言に無い");
    }
    // 破壊系は宣言しない（作業領域の外へ出る操作を誘導しない）。
    for forbidden in ["fs_delete", "shell", "office.live_edit"] {
        assert!(
            !tools.contains(&forbidden),
            "{forbidden} を宣言してはいけない"
        );
    }
}
