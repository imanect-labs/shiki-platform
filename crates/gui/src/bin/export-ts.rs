//! gui の語彙と skill・ミニアプリの本文の型を TypeScript へ書き出す（codegen が正）。
//!
//! workflow-engine の `export-workflow-ts` と同型。scripts/gen-api.sh から呼ぶ。

#![allow(clippy::expect_used, clippy::print_stdout)]

use std::path::PathBuf;

use ts_rs::TS;

use gui::action::{ActionBinding, DataViewBinding, HandlerBinding, WorkflowBinding, WorkflowPin};
use gui::miniapp::{ComponentPin, MiniAppBody, NamedComponentPin};
use gui::skill::{
    FewShotExample, KnowledgeScope, ModelDefaults, ScriptKind, SkillBody, SkillScript,
};
use gui::skill_command::{CommandPhase, SkillCommand, SkillCommandVariant};
use gui::validate::GuiValidationError;
use gui::vocab::HandlerKind;

fn main() {
    let out_dir = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from("web/src/generated"), PathBuf::from);
    std::fs::create_dir_all(&out_dir).expect("出力ディレクトリ作成");

    // 全型を 1 ファイルへ連結する（相互参照のため同一スコープに置く）。
    let mut decls: Vec<String> = Vec::new();
    macro_rules! export {
        ($($t:ty),+ $(,)?) => {
            $( decls.push(format!("export {}", <$t as TS>::decl())); )+
        };
    }
    export!(
        HandlerKind,
        // ツール名語彙は agent-core が単一ソース（skill の allowed_tools が参照する）。
        agent_core::ToolName,
        // ループが横取りするメタツール（plan / tool_search）。表示辞書が引く。
        agent_core::MetaToolName,
        ActionBinding,
        HandlerBinding,
        WorkflowBinding,
        WorkflowPin,
        // 画面の表（data_grid）の保存ビューのデータ束縛（Task 15.7）。
        DataViewBinding,
        GuiValidationError,
        // skill（Task 6.7）とミニアプリ（Task 6.10）の body 型。
        SkillBody,
        KnowledgeScope,
        ModelDefaults,
        FewShotExample,
        SkillScript,
        ScriptKind,
        SkillCommand,
        SkillCommandVariant,
        CommandPhase,
        MiniAppBody,
        ComponentPin,
        NamedComponentPin,
    );

    // 利用者が発話ごとに指定できるツール（#734・agent-core の単一定義）。型だけでなく値の
    // 並びも要る（入力欄のメニューに並べる）ため定数で出し、要素型を `ToolName` に縛る。
    let selectable = agent_core::USER_SELECTABLE_TOOLS
        .iter()
        .map(|t| format!("{:?}", t.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    decls.push(format!(
        "export const USER_SELECTABLE_TOOLS = [{selectable}] as const satisfies readonly ToolName[];\n\n\
         export type UserSelectableTool = (typeof USER_SELECTABLE_TOOLS)[number];"
    ));

    let header = "// 自動生成: cargo run -p shiki-gui --bin export-gui-ts（編集禁止）\n\
                  // 正本: crates/gui/src/{vocab,action,validate,skill,miniapp}.rs・\n\
                  //       crates/agent-core/src/vocab.rs（ToolName / USER_SELECTABLE_TOOLS）\n\n";
    let body = decls.join("\n\n");
    let path = out_dir.join("gui-spec.ts");
    std::fs::write(&path, format!("{header}{body}\n")).expect("gui-spec.ts の書き出し");
    println!("wrote {}", path.display());
}
