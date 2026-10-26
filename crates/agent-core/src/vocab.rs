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
        /// スキルのカタログ引き（name+description 一覧から必要時に instructions を読み込む・
        /// 発話ユーザー権限で解決・発動は run イベントに記録・#344 Task 10.11）。
        Skill => "skill",
        /// スキルの検索（件数が一覧の上限を超えたときだけ提示・name と説明を返す・#520）。
        /// 読み込みは従来どおり `skill`。tool search には統合しない（返すものが指示文で、
        /// 定義の読み込みではないため）。
        SkillSearch => "skill_search",
        DocSearch => "doc_search",
        /// 索引済み文書の本文を窓で読む（検索で当たった文書を引用の外まで読み進める・#425）。
        DocRead => "doc_read",
        /// レポートの引用が証拠台帳に実在するかの機械照合（#409・読み取りのみ）。
        QuoteCheck => "quote_check",
        WebSearch => "web_search",
        WebFetch => "web_fetch",
        CodeInterpreter => "code_interpreter",
        FsList => "fs_list",
        FsRead => "fs_read",
        Grep => "grep",
        FsWrite => "fs_write",
        /// ファイル末尾への追記（証拠台帳のような append-only メモを全文再送なしに伸ばす・#392）。
        FsAppend => "fs_append",
        FsEdit => "fs_edit",
        FsDelete => "fs_delete",
        Shell => "shell",
        /// 調査の**委譲**（隔離コンテキストのサブエージェントを 1 体走らせる・#391）。
        /// 子は新しい履歴・read-only ツールのみで走り、**合成済み findings だけ**を返す。
        Subagent => "subagent",
        /// 撤去した generative UI の発話ツール（#604）。図は回答の本文に ```ui で書く。
        /// 保存済みのスキルの `allowed_tools` が読めるよう**名前だけ**残す（どこにも提示しない）。
        EmitUi => "emit_ui",
        /// ワークフロー IR の生成/更新ツール（保存パイプライン検証・Task 10.13）。
        EmitWorkflow => "emit_workflow",
        /// 既存ワークフロー IR の読み取りツール（AI 編集の前提・Task 10.13）。
        ReadWorkflow => "read_workflow",
        /// ノート（md/Yjs）の共同編集ツール（AI が編集参加者として編集・Task 11P.4）。
        DocumentEdit => "document.edit",
        /// ノート本文の読み取りツール（document.edit の前提・現状の md を得る・Task 11P.4）。
        DocumentRead => "document.read",
        /// ノート本文への genui 埋め込み挿入ツール（グラフ等・非破壊 append・確認不要・issue #282）。
        DocumentEmbed => "document.embed",
        /// AI 生成 md を新規ノートとして保存するツール（note_ref カード化・Task 11P.5）。
        SaveNote => "save_note",
        /// AI 生成スライドを**下書き**として用意するツール（slide_draft カード化・保存しない・
        /// 確認不要・save_note と同型の下書き確定型・Task 11.3）。
        SaveSlide => "save_slide",
        /// AI 生成 CSV を**下書き**として用意するツール（csv_draft カード化・保存しない・
        /// 確認不要・save_note と同型の下書き確定型・Task 11.11）。
        SaveCsv => "save_csv",
        /// AI 生成スキルを**下書き**として用意するツール（skill_draft カード化・保存しない・
        /// 確認不要・save_note と同型の下書き確定型・#919）。保存はスキルエディタで人が行う。
        SaveSkill => "save_skill",
        /// 新規 Word 文書（.docx）を作成するツール（#381）。空テンプレを実体化してから
        /// Collabora へ本文を paste する（md エディタ下書きは廃止・承認ゲート対象）。
        SaveDocument => "save_document",
        /// 新規 Excel ブック（.xlsx）を作成するツール（#381）。空テンプレを実体化してから
        /// Collabora Calc へ値の矩形を貼り込む（承認ゲート対象）。
        SaveSheet => "save_sheet",
        /// スライド（.slide/Yjs）の共同編集ツール（AI が編集参加者・排他なし・Task 11.3）。
        SlideEdit => "slide.edit",
        /// スライド内容の読み取りツール（slide.edit の前提・正規化 JSON を得る・Task 11.3）。
        SlideRead => "slide.read",
        /// Office ファイル（docx/xlsx/pptx）の AI 編集（非ロック時=新バージョン/
        /// WOPI ロック中=提案バージョン・Task 11.8）。
        OfficeEdit => "office.edit",
        /// Office 文書への AI ライブ編集（AI が CoolWSD セッションの headless 参加者
        /// として接続し、自 view でアンカー指定編集・全参加者へ即反映・issue #352）。
        OfficeLiveEdit => "office.live_edit",
        /// Excel・Word の読み取り（Excel は値・数式・書式・結合・列幅・付帯情報、Word は
        /// ブロック一覧と書式つき HTML。#725）。
        OfficeRead => "office.read",
        /// Excel ブックのレイアウトまで含む編集（kit 内の固定エンジン・全部成功か全部取り消し。#725）。
        OfficeExcelEdit => "office.excel_edit",
        /// Word 文書のレイアウトまで含む編集（HTML の差し込み・スタイル・ページ設定・目次。
        /// kit 内の固定エンジン・全部成功か全部取り消し。#725）。
        OfficeWordEdit => "office.word_edit",
        /// CSV への読み取り専用 SQL クエリ（隔離 DuckDB 経由・Task 11P.9）。
        CsvQuery => "csv.query",
        /// CSV のパッチ編集→新バージョン（editor・Task 11P.9）。
        CsvPatch => "csv.patch",
        /// 新規 CSV の保存（作成権限・Task 11P.9）。
        CsvWrite => "csv.write",
        /// 構造化データ（テーブル）の検索・集計（`data::DataQuery`・本人の行述語/マスク・Task 15.7）。
        DataQuery => "data_query",
        /// テーブルの列（参照名・型・選択肢）のグラウンディング（読めない列は載せない・Task 15.7）。
        DataSchema => "data_schema",
        /// テーブルのレコードの作成・更新（editor ＋書込述語・承認必須・Task 15.7）。
        DataRecordWrite => "data_record_write",
    }
}

vocab_enum! {
    /// ループが**横取りする**メタツールの名前（`Tool` として dispatch されない・[`ToolName`] の外）。
    ///
    /// モデルには通常のツールとして見えるため、UI の表示辞書もこの名前で引く（TS へ生成する）。
    pub enum MetaToolName {
        /// 計画の提示/改訂（自律プロファイル・[`crate::agent`]）。
        Plan => "plan",
        /// 遅延ツールの検索と読み込み（[`crate::tool_search`]）。
        ToolSearch => "tool_search",
    }
}

/// ツール定義をモデルの文脈へ載せる時機（tool search・[`crate::tool_search`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolLoading {
    /// 常に載せる（ほぼ毎回使う・検索の 1 手を挟む方が高くつく）。
    Eager,
    /// `tool_search` で見つかるまで載せない（使う場面が限られる・定義が大きい）。
    Deferred,
}

/// 利用者が**発話ごとに指定できる**ツール（チャット入力欄の「ツール」・#734）。
///
/// 指定は誘導（tool search の常時ロード＋system の「ユーザーが明示的に指定した」注記）で
/// あって権限ではないが、注記はモデルの判断を強く寄せる。破壊系・内部用（`fs_delete` /
/// `shell` / `emit_workflow` / `save_*` 等）をユーザーの明示指定として格上げしないよう、
/// 指定できる集合をここで閉じる。API はこの外を 400 で拒み、web の選択肢はこの定数から
/// 生成した TS（`gui-spec.ts` の `USER_SELECTABLE_TOOLS`）で束縛する（手書きの対応表を作らない）。
pub const USER_SELECTABLE_TOOLS: &[ToolName] = &[
    ToolName::DocSearch,
    ToolName::WebSearch,
    ToolName::CodeInterpreter,
];

impl ToolName {
    /// 利用者が発話ごとに指定できるツールか（[`USER_SELECTABLE_TOOLS`]）。
    #[must_use]
    pub fn is_user_selectable(self) -> bool {
        USER_SELECTABLE_TOOLS.contains(&self)
    }

    /// 既定のロード区分。**網羅 match** なので、新ツールは追加時にここで区分を決める。
    ///
    /// 常時ロードの基準: 会話・調査・作業ファイルの**基本動作**（毎 run のように使う）。
    /// 遅延の基準: 特定の文書種別（ノート/スライド/Office/CSV）やワークフローに紐づくもの、
    /// 破壊的で出番の少ないもの。run 単位の上書きは [`crate::profile::ToolSearchOptions`]。
    #[must_use]
    pub const fn loading(self) -> ToolLoading {
        match self {
            Self::Skill
            | Self::SkillSearch
            | Self::DocSearch
            | Self::DocRead
            | Self::QuoteCheck
            | Self::WebSearch
            | Self::WebFetch
            | Self::CodeInterpreter
            | Self::FsList
            | Self::FsRead
            | Self::Grep
            | Self::FsWrite
            | Self::FsAppend
            | Self::FsEdit
            | Self::Subagent
            | Self::EmitUi
            // 構造化データの読取は「社内の知識源」として doc_search と同じく常時載せる（発話ユーザーに
            // 見えるテーブルが 1 つも無ければ提示しない・description のテーブル一覧は有界）。
            | Self::DataQuery
            | Self::DataSchema => ToolLoading::Eager,
            Self::FsDelete
            | Self::Shell
            | Self::EmitWorkflow
            | Self::ReadWorkflow
            | Self::DocumentEdit
            | Self::DocumentRead
            | Self::DocumentEmbed
            | Self::SaveNote
            | Self::SaveSlide
            | Self::SaveCsv
            | Self::SaveSkill
            | Self::SaveDocument
            | Self::SaveSheet
            | Self::SlideEdit
            | Self::SlideRead
            | Self::OfficeEdit
            | Self::OfficeLiveEdit
            | Self::OfficeRead
            | Self::OfficeExcelEdit
            | Self::OfficeWordEdit
            | Self::CsvQuery
            | Self::CsvPatch
            | Self::CsvWrite
            | Self::DataRecordWrite => ToolLoading::Deferred,
        }
    }

    /// tool search の索引に足す検索語（description に現れない言い換え・英語の別名）。
    ///
    /// description は日本語だが、モデルは英語で検索することが多い。ここで橋を架ける。
    #[must_use]
    pub const fn search_keywords(self) -> &'static str {
        match self {
            Self::FsDelete => "delete remove file 削除 消去",
            Self::Shell => "shell bash command terminal run コマンド 実行 端末",
            Self::EmitWorkflow => "workflow automation create update ワークフロー 自動化 作成",
            Self::ReadWorkflow => "workflow read inspect ワークフロー 読む 確認",
            Self::DocumentEdit => {
                "note markdown document edit rewrite append ノート 文書 編集 追記 書き換え"
            }
            Self::DocumentRead => "note markdown document read ノート 文書 本文 読む",
            Self::DocumentEmbed => "note chart graph embed ノート グラフ 図 埋め込み",
            Self::SaveNote => "note markdown create save draft ノート メモ 作成 保存",
            Self::SaveSlide => "slide presentation deck create draft スライド プレゼン 資料 作成",
            Self::SaveCsv => "csv table spreadsheet create draft 表 作成",
            Self::SaveSkill => {
                "skill prompt instructions create draft edit スキル 指示文 プロンプト 作成 編集"
            }
            Self::SaveDocument => "word docx document create report 文書 ワード 報告書 作成",
            Self::SaveSheet => {
                "excel xlsx spreadsheet sheet workbook create 表計算 エクセル スプレッドシート 作成"
            }
            Self::SlideEdit => {
                "slide presentation deck edit rewrite スライド プレゼン 編集 書き換え"
            }
            Self::SlideRead => "slide presentation deck read スライド プレゼン 読む",
            Self::OfficeEdit => {
                "office word excel powerpoint docx xlsx pptx edit ワード エクセル パワポ 編集"
            }
            Self::OfficeLiveEdit => {
                "office word excel powerpoint live collaborative edit ワード エクセル 共同編集"
            }
            Self::OfficeRead => {
                "excel xlsx spreadsheet workbook word docx document read cells formula format layout \
                 エクセル 表計算 ワード 文書 読む セル 数式 書式 段落 見出し"
            }
            Self::OfficeExcelEdit => {
                "excel xlsx spreadsheet edit format chart table formula conditional formatting print \
                 エクセル 表計算 編集 書式 罫線 グラフ 数式 条件付き書式 印刷 列幅 結合"
            }
            Self::OfficeWordEdit => {
                "word docx document report edit heading table style layout header footer toc page \
                 ワード 文書 報告書 編集 見出し 表 スタイル 書式 ヘッダー フッター 目次 ページ設定"
            }
            Self::CsvQuery => "csv sql query table select aggregate 表 集計 検索",
            Self::CsvPatch => "csv table edit update row cell 表 行 セル 編集 更新",
            Self::CsvWrite => "csv table create write save 表 作成 保存",
            Self::DataRecordWrite => {
                "data table record row create update write insert テーブル レコード 行 追加 登録 更新 書き込み"
            }
            Self::Skill
            | Self::SkillSearch
            | Self::DocSearch
            | Self::DocRead
            | Self::QuoteCheck
            | Self::WebSearch
            | Self::WebFetch
            | Self::CodeInterpreter
            | Self::FsList
            | Self::FsRead
            | Self::Grep
            | Self::FsWrite
            | Self::FsAppend
            | Self::FsEdit
            | Self::Subagent
            | Self::EmitUi
            | Self::DataQuery
            | Self::DataSchema => "",
        }
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
        // 撤去したツールの名前も、保存済みのスキルが読めるよう残す（#604）。
        let t: ToolName = serde_json::from_str("\"emit_ui\"").unwrap();
        assert_eq!(t, ToolName::EmitUi);
    }

    /// 発話ごとに指定できる集合は重複なく、破壊系・内部用を含まない（#734）。
    #[test]
    fn user_selectable_tools_exclude_destructive_and_internal() {
        for (i, t) in USER_SELECTABLE_TOOLS.iter().enumerate() {
            assert!(!USER_SELECTABLE_TOOLS[..i].contains(t), "{t:?} が重複");
            assert!(t.is_user_selectable());
        }
        for denied in [
            ToolName::FsDelete,
            ToolName::Shell,
            ToolName::EmitWorkflow,
            ToolName::FsWrite,
            ToolName::SaveNote,
            ToolName::SaveDocument,
            ToolName::CsvWrite,
            ToolName::OfficeEdit,
            ToolName::Skill,
            ToolName::Subagent,
            ToolName::EmitUi,
        ] {
            assert!(!denied.is_user_selectable(), "{denied:?} は指定できない");
        }
        assert!(ToolName::WebSearch.is_user_selectable());
    }
}
