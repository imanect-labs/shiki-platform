//! ツール名語彙の単一ソース（Phase 6 Task 6.9 の前提）。
//!
//! ツール名は LLM への提示・承認ポリシ・skill の許可ツール・generative UI のアクション束縛が
//! 共有する認可語彙であり、文字列リテラルの散在は閉じた集合の照合（ハルシネーション境界）を
//! 壊す。workflow-engine / authz の vocab と同型の `vocab_enum!` で Rust enum を正とし、
//! `#[derive(TS)]` で TypeScript 型を生成する（codegen が正・手書きミラー禁止）。

/// variant と serde/TS 名の対応を単一定義から生成する（as_str/parse の乖離を構造的に防ぐ）。
/// workflow-engine の `vocab_enum!` と同型（クレート間で macro を共有せず同型を保つ）。
macro_rules! vocab_enum {
    (
        $(#[$attr:meta])*
        $vis:vis enum $enum_name:ident {
            $( $(#[$vattr:meta])* $variant:ident => $name:literal, )+
        }
    ) => {
        $(#[$attr])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash,
            serde::Serialize, serde::Deserialize, ts_rs::TS,
        )]
        #[ts(export)]
        $vis enum $enum_name {
            $( $(#[$vattr])* #[serde(rename = $name)] $variant, )+
        }

        impl $enum_name {
            /// serde/TS/LLM 提示で共通の文字列表現。
            $vis const fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $name, )+ }
            }

            /// 文字列から閉集合へ（未知は None・fail-closed）。
            $vis fn parse(s: &str) -> Option<Self> {
                match s { $( $name => Some(Self::$variant), )+ _ => None }
            }

            /// 全 variant（カタログ列挙・roundtrip テスト用）。
            $vis const ALL: &'static [$enum_name] = &[ $( Self::$variant, )+ ];
        }
    };
}

vocab_enum! {
    /// agent-core が提供する全ツールの名前（閉じた集合）。
    ///
    /// 新ツールはここへ variant を足し、`Tool::name()` は `as_str()` を返す。
    /// skill の許可ツール・UI アクションの tool 束縛はこの語彙へ照合して未知名を弾く。
    pub enum ToolName {
        DocSearch => "doc_search",
        WebSearch => "web_search",
        WebFetch => "web_fetch",
        CodeInterpreter => "code_interpreter",
        FsList => "fs_list",
        FsRead => "fs_read",
        Grep => "grep",
        FsWrite => "fs_write",
        FsEdit => "fs_edit",
        FsDelete => "fs_delete",
        Shell => "shell",
        /// generative UI スペックの発話ツール（Phase 6 Task 6.4）。
        EmitUi => "emit_ui",
        /// ワークフロー IR の生成/更新ツール（保存パイプライン検証・Task 10.13）。
        EmitWorkflow => "emit_workflow",
        /// 既存ワークフロー IR の読み取りツール（AI 編集の前提・Task 10.13）。
        ReadWorkflow => "read_workflow",
        /// ノート（md/Yjs）の共同編集ツール（AI が編集参加者として編集・Task 11P.4）。
        DocumentEdit => "document.edit",
        /// ノート本文の読み取りツール（document.edit の前提・現状の md を得る・Task 11P.4）。
        DocumentRead => "document.read",
        /// AI 生成 md を新規ノートとして保存するツール（note_ref カード化・Task 11P.5）。
        SaveNote => "save_note",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_tool_names() {
        for t in ToolName::ALL {
            assert_eq!(ToolName::parse(t.as_str()), Some(*t));
        }
        assert_eq!(ToolName::parse("bogus_tool"), None);
    }

    #[test]
    fn serde_matches_as_str() {
        assert_eq!(
            serde_json::to_string(&ToolName::DocSearch).unwrap(),
            "\"doc_search\""
        );
        let t: ToolName = serde_json::from_str("\"emit_ui\"").unwrap();
        assert_eq!(t, ToolName::EmitUi);
    }
}
