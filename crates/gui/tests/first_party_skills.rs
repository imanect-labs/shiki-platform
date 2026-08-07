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

/// grilling は**質問ラウンドを何度も回す**のが本体なので、それを可能にする宣言を固定する（#428）。
///
/// `phase` を宣言すると実行前フェーズの門が掛かり（`crates/chat/src/worker/gate.rs`）、
/// grilling とは 2 箇所で噛み合わない。
///
/// - **調べられない**: `Clarify` 段階で渡るツールは `emit_ui` だけ（`allows`）。`subagent` も
///   `doc_search` も無いので、1 ラウンド目は調べずに質問を書くしかなくなる。
/// - **質問できない**: 最初の質問カードでスレッドは `Plan` へ移り（`stage_from_cards`）、以降
///   `emit_ui` は `plan_card` しか通さない（`allowed_cards`）。2 ラウンド目が出せない。
///
/// 「調べられるようになった頃には質問カードが出せない」順序になるため、宣言しないことが仕様。
/// うっかり `phase` を足すと静かに壊れるので、その不在を固定する。
#[test]
fn grilling_keeps_rounds_open_and_declares_safe_tools() {
    let (_, body) = bundles()
        .into_iter()
        .find(|(name, _)| name == "grilling")
        .expect("grilling バンドルがあること");
    let skill = gui::validate_skill_body(&body).expect("検証を通る");

    let command = skill.command.expect("command 宣言がある");
    assert_eq!(command.name, "grill");
    let args: Vec<&str> = command.variants.iter().map(|v| v.args.as_str()).collect();
    assert_eq!(
        args,
        vec!["", "quick"],
        "既定（詰め切る）と quick（1 ラウンドで切り上げ）の 2 経路"
    );
    for variant in &command.variants {
        assert!(
            variant.phase.is_none(),
            "{}: phase を宣言すると 2 ラウンド目の質問カードが出せなくなる",
            variant.args
        );
    }

    // 面接に要る宣言: 質問/計画カード・事実調べの委譲・台帳への追記・共通理解の保存。
    let tools: Vec<&str> = skill
        .allowed_tools
        .as_ref()
        .expect("allowed_tools を宣言する")
        .iter()
        .map(|t| t.as_str())
        .collect();
    for expected in ["emit_ui", "subagent", "fs_append", "fs_read", "save_note"] {
        assert!(tools.contains(&expected), "{expected} が宣言に無い");
    }
    // 破壊系は宣言しない（deep-research と同じ禁止則）。
    for forbidden in ["fs_delete", "shell", "office.live_edit"] {
        assert!(
            !tools.contains(&forbidden),
            "{forbidden} を宣言してはいけない"
        );
    }

    // 上流（mattpocock/skills・MIT）の派生物なので、**配布物そのもの**が帰属を持ち歩く必要がある。
    // import が artifact 化するのは body だけで隣の NOTICE は同行しないため、instructions 末尾の
    // 表示が唯一の同梱経路になる（消すとライセンス条件を満たさなくなる）。
    //
    // MIT が要求するのは「著作権表示**と許諾文**の同梱」なので、見出しと著作権行だけでなく
    // **許諾・再配布条件・無保証条項の実文**まで固定する（本文だけ削られても落ちるように）。
    for required in [
        "MIT License",
        "Copyright (c) 2026 Matt Pocock",
        "https://github.com/mattpocock/skills",
        "Permission is hereby granted, free of charge",
        "shall be included in all copies or\nsubstantial portions of the Software",
        "WITHOUT WARRANTY OF ANY KIND",
    ] {
        assert!(
            skill.instructions.contains(required),
            "instructions に上流のライセンス条項（{required:?}）が無い"
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
        // 委譲（#391）。抜けるとモデルが「調査を分けて並列に走らせる」選択肢を見失う。
        "subagent",
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
