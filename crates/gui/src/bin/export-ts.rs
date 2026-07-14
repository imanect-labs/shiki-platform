//! generative UI のカタログ・スペック型を TypeScript へ書き出す（codegen が正）。
//!
//! workflow-engine の `export-workflow-ts` と同型。scripts/gen-api.sh から呼ぶ。

#![allow(clippy::expect_used, clippy::print_stdout)]

use std::path::PathBuf;

use ts_rs::TS;

use gui::action::{ActionBinding, HandlerBinding, ToolBinding, WorkflowBinding, WorkflowPin};
use gui::chart::{ChartPoint, ChartSpec};
use gui::form_fields::{CheckboxGroupProps, DateProps, RadioGroupProps, RatingProps, SliderProps};
use gui::layout::{
    AccordionItem, AccordionProps, BadgeItem, BadgeListProps, BadgeTone, CalloutProps, CalloutTone,
    CodeBlockProps, KeyValueItem, KeyValueProps, StepItem, StepStatus, StepperProps, TabItem,
    TabsProps,
};
use gui::map::{GeoBounds, GeoPoint, MapMarker, MapProps, MapRoute, MarkerKind, RouteMode};
use gui::miniapp::{ComponentPin, MiniAppBody, NamedComponentPin};
use gui::question::{QuestionCardProps, QuestionItem, QuestionOption};
use gui::skill::{
    FewShotExample, KnowledgeScope, ModelDefaults, ScriptKind, SkillBody, SkillScript,
};
use gui::spec::{
    ActionRef, ButtonProps, ButtonVariant, CellAlign, CellValue, ContainerProps, FormField,
    FormProps, Layout, LinkProps, ReservedProps, SelectOption, SelectProps, StatProps, TableColumn,
    TableProps, TextInputProps, TextProps, TextVariant, UiNode, UiSpecDoc,
};
use gui::validate::GuiValidationError;
use gui::vocab::{ChartKind, ComponentKind, HandlerKind};

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
        ComponentKind,
        HandlerKind,
        ChartKind,
        // ツール名語彙は agent-core が単一ソース（アクション束縛が参照する）。
        agent_core::ToolName,
        UiSpecDoc,
        UiNode,
        ActionRef,
        ContainerProps,
        Layout,
        TextProps,
        TextVariant,
        LinkProps,
        FormProps,
        FormField,
        TextInputProps,
        SelectProps,
        SelectOption,
        CheckboxGroupProps,
        RadioGroupProps,
        DateProps,
        SliderProps,
        RatingProps,
        ButtonProps,
        ButtonVariant,
        TableProps,
        TableColumn,
        CellAlign,
        CellValue,
        ReservedProps,
        ChartSpec,
        ChartPoint,
        StatProps,
        CalloutProps,
        CalloutTone,
        AccordionProps,
        AccordionItem,
        TabsProps,
        TabItem,
        StepperProps,
        StepItem,
        StepStatus,
        BadgeListProps,
        BadgeItem,
        BadgeTone,
        KeyValueProps,
        KeyValueItem,
        CodeBlockProps,
        QuestionCardProps,
        QuestionItem,
        QuestionOption,
        MapProps,
        GeoPoint,
        MapMarker,
        MarkerKind,
        MapRoute,
        RouteMode,
        GeoBounds,
        ActionBinding,
        ToolBinding,
        HandlerBinding,
        WorkflowBinding,
        WorkflowPin,
        GuiValidationError,
        // skill（Task 6.7）とミニアプリ（Task 6.10）の body 型。
        SkillBody,
        KnowledgeScope,
        ModelDefaults,
        FewShotExample,
        SkillScript,
        ScriptKind,
        MiniAppBody,
        ComponentPin,
        NamedComponentPin,
    );

    let header = "// 自動生成: cargo run -p shiki-gui --bin export-gui-ts（編集禁止）\n\
                  // 正本: crates/gui/src/{vocab,spec,chart,action,validate}.rs\n\n";
    let body = decls.join("\n\n");
    let path = out_dir.join("gui-spec.ts");
    std::fs::write(&path, format!("{header}{body}\n")).expect("gui-spec.ts の書き出し");
    println!("wrote {}", path.display());
}
