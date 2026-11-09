//! ノードの出力の形の単一定義（行数ゲート対応で catalog.rs から分離）。
//!
//! AI の説明文と右パネルの「前のノードの出力」が読む（[`super::NodeCatalogEntry::output_shape`]・`output_fields`）。

use super::{NodeType, OutputField};

/// 出力の形（項目の組か、項目に分けられない形の文）。
pub(super) enum Shape {
    /// 項目の組（前置き・最上位の項目・後置き）。表記は `前置き{ a: T, b?: U（補足） }後置き`。
    Fields(&'static str, &'static [OutputField], &'static str),
    /// 項目に分けられない形（入力をそのまま通す・戻り値そのものなど）。予約語彙は空文字。
    Prose(&'static str),
}

impl Shape {
    /// 人と AI が読む TS 風の表記（項目の組は項目から作る・手書きの文字列を別に持たない）。
    pub(super) fn render(&self) -> String {
        match self {
            Shape::Prose(text) => (*text).to_string(),
            Shape::Fields(prefix, fields, suffix) => {
                let items: Vec<String> = fields
                    .iter()
                    .map(|f| {
                        let opt = if f.optional { "?" } else { "" };
                        let note = if f.note.is_empty() {
                            String::new()
                        } else {
                            format!("（{}）", f.note)
                        };
                        format!("{}{opt}: {}{note}", f.name, f.ty)
                    })
                    .collect();
                format!("{prefix}{{ {} }}{suffix}", items.join(", "))
            }
        }
    }

    pub(super) fn fields(&self) -> &'static [OutputField] {
        match self {
            Shape::Fields(_, fields, _) => fields,
            Shape::Prose(_) => &[],
        }
    }
}

/// 出力の項目 1 つ（`field!("名前", "型")`・`field!("名前", "型", "補足")`）。`opt!` は省略されうる項目。
macro_rules! field {
    ($name:literal, $ty:literal) => {
        OutputField {
            name: $name,
            ty: $ty,
            optional: false,
            note: "",
            present_if: "",
        }
    };
    ($name:literal, $ty:literal, $note:literal) => {
        OutputField {
            name: $name,
            ty: $ty,
            optional: false,
            note: $note,
            present_if: "",
        }
    };
}
macro_rules! opt {
    ($name:literal, $ty:literal) => {
        OutputField {
            name: $name,
            ty: $ty,
            optional: true,
            note: "",
            present_if: "",
        }
    };
    ($name:literal, $ty:literal, $note:literal) => {
        OutputField {
            name: $name,
            ty: $ty,
            optional: true,
            note: $note,
            present_if: "",
        }
    };
    // 設定のこの場所に値があるときだけ入る項目（[`OutputField::present_if`]）。
    ($name:literal, $ty:literal, $note:literal, present_if = $at:literal) => {
        OutputField {
            name: $name,
            ty: $ty,
            optional: true,
            note: $note,
            present_if: $at,
        }
    };
}

/// 出力の形（ノード種ごと・exhaustive）。AI の説明文と右パネルの「前のノードの出力」の単一定義。
///
/// 出力は能力ポート（api の `ProdNodePorts`・storage・data）と executor が組む JSON で、型としての
/// 定義を持たない。ここを変えずに出力を変えると AI が誤った path を書くので、出力側を変えたら
/// ここも直す。予約語彙は空文字（実装時に足す）。
#[allow(clippy::too_many_lines)] // ノード種ごとの形の表（exhaustive に並べる方が漏れを見つけやすい）。
pub(super) fn output_shape(nt: NodeType) -> Shape {
    use NodeType as N;
    use Shape::{Fields, Prose};
    match nt {
        N::ControlBranch | N::ControlSwitch | N::ControlJoin => {
            Prose("実行の入力をそのまま通す（分岐・合流の結果としては参照しない）")
        }
        N::ControlMap => Fields(
            "",
            &[
                field!("items", "Array<unknown>", "領域の出口ノードの出力・要素の順"),
                opt!(
                    "errors",
                    "Array<{ index: number, error: { code: string, message: string } }>",
                    "on_item_error: collect のとき"
                ),
            ],
            "",
        ),
        N::ControlWait => Prose("event なら届いたできごとの内容・時間待ちは null"),
        N::StorageRead => Fields(
            "",
            &[
                field!("file_id", "string", "読んだファイルの ID"),
                field!("id", "string", "file_id と同じ・古い名前"),
                field!("name", "string"),
                field!("version", "number", "読んだバージョン・CSV を編集の前提にできる"),
                field!("content_type", "string"),
                field!("size", "number"),
                opt!(
                    "encoding",
                    "string",
                    "読んだ文字コード（UTF-8・Shift_JIS・EUC-JP・UTF-16LE・UTF-16BE）・文章のファイルのときだけ"
                ),
                field!(
                    "text",
                    "string",
                    "中身の文章（先頭の BOM は含まない）・PDF や Word は社内文書の検索に取り込まれた本文で、ページの変わり目に〔p.2〕のような印と、見出しの行（# で始まる）が入る"
                ),
                field!(
                    "truncated",
                    "boolean",
                    "PDF や Word の本文が長く、上限の字数で切ったとき true・文章のファイルは切らない（いつも false）"
                ),
            ],
            "",
        ),
        N::StorageWrite => Fields(
            "",
            &[
                field!("file_id", "string", "保存したファイルの ID"),
                field!("id", "string", "file_id と同じ・古い名前"),
                field!("name", "string", "保存した名前・番号を付けたときは付けた名前"),
                field!("version", "number"),
                field!("sha256", "string"),
                field!("size", "number"),
            ],
            "",
        ),
        // 件数の上限は vocab::STORAGE_LIST_MAX_ITEMS（試験で文と照らす・BUG-180）。続きの位置は D-131。
        N::StorageList => Fields(
            "",
            &[
                field!(
                    "items",
                    "Array<{ id: string, name: string, kind: string, size: number }>",
                    "名前の順・最大 100 件・kind は file か folder"
                ),
                field!(
                    "truncated",
                    "boolean",
                    "続きがあるとき true・残りは items に入らない"
                ),
                field!(
                    "next_cursor",
                    "string | null",
                    "続きの位置・同じフォルダーで cursor に渡すと続きを読む・続きが無ければ null"
                ),
            ],
            "",
        ),
        N::RagSearch => Fields(
            "",
            &[field!(
                "results",
                "Array<{ file_id: string, file_name: string, content: string, relevance: \"high\" | \"medium\" | \"low\", score: number }>",
                "関連の強いものから並ぶ・relevance は関連の強さの目安・score は順位から決めた値（1 位は 1、2 位は 0.5）で関連の強さではない"
            )],
            "",
        ),
        N::LlmInvoke => Fields(
            "",
            &[
                field!("text", "string"),
                field!("model", "string"),
                opt!(
                    "data",
                    "unknown",
                    "output_schema を書いたとき・その形で検証済みの値",
                    present_if = "/output_schema"
                ),
            ],
            "",
        ),
        N::AgentInvoke => Fields(
            "",
            &[
                field!("text", "string", "最後の回答・Markdown"),
                field!("complete", "boolean", "false は上限で打ち切った回答"),
                field!(
                    "stop",
                    "\"completed\" | \"truncated\" | \"budget_steps\" | \"budget_time\" | \"budget_tokens\" | \"budget_cost\""
                ),
                field!("model", "string"),
                field!(
                    "citations",
                    "Array<{ n: number, file_id?: string, file_name?: string, url?: string, title?: string, snippet: string }>",
                    "使った資料・50 件まで"
                ),
                field!("tool_calls", "Array<{ tool: string, ok: boolean }>"),
                field!("skipped_tools", "Array<string>", "指定したが使えなかった道具"),
                field!(
                    "usage",
                    "{ steps: number, tokens: number, cost_usd_micros: number }"
                ),
            ],
            "",
        ),
        N::AiDecide => Fields(
            "",
            &[
                field!(
                    "answers",
                    "{ [質問のキー]: { type: \"choice\", value: string, label: string, confidence: number, probabilities: { [選択肢のキー]: number } } | { type: \"score\", value: number, level: string, level_index: number, confidence: number, probabilities: Array<number> } | { type: \"boolean\", value: boolean, probability: number, confidence: number } }",
                    "確からしさ・確率は 0〜1。choice の label は選択肢の名前（無ければ説明、それも無ければキー）"
                ),
                field!("model", "string", "指定した判定用のモデル"),
                field!("resolved_model", "string", "実際に答えた決定器の版"),
                field!(
                    "provider",
                    "string | null",
                    "答えた決定器（llm_decider・typesafe・stub）"
                ),
                field!(
                    "calibrated",
                    "boolean",
                    "false は確からしさが較正されていない（AI の自己申告・得票率）"
                ),
                field!("method", "\"native\" | \"verbalized\" | \"vote\""),
                field!("samples", "number", "1 回の判定で答えさせた回数"),
                field!(
                    "cached",
                    "boolean",
                    "決定キャッシュの答えを使い回したか（ノードに cache を書いたときだけ true になり得る）"
                ),
                field!(
                    "routed",
                    "{ question: string, port: string } | null",
                    "route を書いたとき・進んだ出口"
                ),
                field!(
                    "raised_questions",
                    "Array<string>",
                    "AI が自分の答えに一様（はい/いいえなら 50%）より低い自信を付け、一様まで引き上げた質問のキー（質問の順・AI の自己申告のときだけ入りうる・無ければ空）"
                ),
            ],
            "",
        ),
        N::HttpRequest => Fields(
            "",
            &[
                field!("status", "number"),
                field!("host", "string"),
                field!("body", "unknown", "JSON なら解析した値・それ以外は文字列"),
            ],
            "",
        ),
        N::ScriptRun => Prose("main(input) の戻り値そのもの"),
        N::WorkflowStart => Fields(
            "",
            &[field!("run_id", "string | null")],
            "（子の結果は待たない）",
        ),
        N::WorkflowReply => Fields(
            "画面に出す節 ",
            &[
                field!(
                    "format",
                    "\"text\" | \"markdown\" | \"table\" | \"file_link\""
                ),
                opt!("label", "string"),
                field!("tone", "string"),
                field!("truncated", "boolean"),
                opt!("text", "string", "text・markdown"),
                opt!("columns", "Array<string>"),
                opt!("rows", "Array<Array<string>>"),
                opt!("total_rows", "number", "table"),
                opt!("files", "Array<{ id: string, name: string }>", "file_link"),
            ],
            "（後続から参照するものではない）",
        ),
        N::SkillInvoke => Prose(
            "script を持つ skill は main の戻り値・指示だけの skill は { text: string, model: string }",
        ),
        N::CsvQuery => Fields(
            "",
            &[
                field!("columns", "Array<string>", "列の名前・同じ名前の 2 つ目からは a_1 のように付け直す"),
                field!(
                    "column_types",
                    "Array<string>",
                    "列の型の名前・BIGINT（整数）・DOUBLE（数値）・BOOLEAN（はい／いいえ）・VARCHAR（テキスト）・DATE（日付）など"
                ),
                field!(
                    "rows",
                    "Array<Array<string | null>>",
                    "値は列の種類にかかわらずテキスト・空は null・日付は 2024-01-31 の形"
                ),
                field!(
                    "items",
                    "Array<{ [列の名前]: string | number | boolean | null }>",
                    "1 行ずつ列の名前で値を引く・数値とはい／いいえと空は種類のまま・日付と時刻は rows と同じテキスト・項目の並びは列の順とは限らない"
                ),
                field!("total_rows", "number | null", "結果の行の数・打ち切ったときは null"),
                field!("truncated", "boolean", "10,000 行か、結果の大きさの上限で打ち切ったとき true"),
                field!("version", "number", "集計した CSV のバージョン・CSV を編集の前提にできる"),
            ],
            "",
        ),
        N::CsvPatch => Fields(
            "",
            &[
                field!("file_id", "string", "編集した CSV の ID"),
                field!("node_id", "string", "file_id と同じ・古い名前"),
                field!("version", "number", "編集の後のバージョン"),
                field!("rows", "number"),
                field!("cols", "number"),
            ],
            "",
        ),
        N::CsvWrite => Fields(
            "",
            &[
                field!("file_id", "string", "保存した CSV の ID"),
                field!("node_id", "string", "file_id と同じ・古い名前"),
                field!("version", "number"),
                field!("name", "string", "保存した名前・番号を付けたときは付けた名前"),
            ],
            "",
        ),
        N::DataQuery => Fields(
            "",
            &[
                opt!(
                    "items",
                    "Array<{ id: string, data: { [列の参照名]: unknown }, rev: number, … }>"
                ),
                // 集計の 1 行は data の `AggregateGroup` をそのまま出す（ノードで詰め替えない・GAP-132）。まとめた列の
                // 値は文字（数の列も）、件数は整数。行の形を言わないと、AI が `/item/<列>`・`/item/count` と書いて
                // 解決できない。
                opt!(
                    "groups",
                    "Array<{ key: { [まとめた列の参照名]: string | null }, value: number | null, values: Array<number | null> }>",
                    "aggregate のとき。values は aggregate と同じ順の値・value は values の先頭。件数が少なくて伏せた行は value と values が null",
                    present_if = "/query/aggregate"
                ),
                opt!("next_cursor", "string"),
                opt!(
                    "count",
                    "{ exact: number } | { at_least: number }",
                    "with_count のとき",
                    present_if = "/query/with_count"
                ),
            ],
            "",
        ),
        N::DataRecordCreate | N::DataRecordUpdate | N::DataTransition => Fields(
            "レコード ",
            &[
                field!("id", "string"),
                field!("table_id", "string"),
                field!("data", "{ [列の参照名]: unknown }"),
                field!("rev", "number"),
                field!("owner", "string"),
                field!("created_at", "string"),
                field!("updated_at", "string"),
            ],
            "",
        ),
        N::DataRecordUpsert => Fields(
            "レコード ",
            &[
                field!("id", "string"),
                field!("table_id", "string"),
                field!("data", "{ [列の参照名]: unknown }"),
                field!("rev", "number"),
                field!("owner", "string"),
                field!("created_at", "string"),
                field!("updated_at", "string"),
                field!("created", "boolean", "追加したら true、書き換えたら false"),
            ],
            "",
        ),
        N::DataRecordDelete => Fields(
            "",
            &[field!("id", "string"), field!("deleted", "true")],
            "",
        ),
        N::DataBulkUpsert => Fields(
            "",
            &[
                field!("import_id", "string"),
                field!("status", "string"),
                field!("rows_total", "number"),
                field!("rows_applied", "number"),
                field!("rows_invalid", "number"),
                field!("rows_rejected", "number"),
                field!(
                    "errors",
                    "Array<{ row: number, key: string | null, message: string }>"
                ),
                field!("last_error", "string | null"),
                opt!(
                    "rows_matched",
                    "number",
                    "同じレコードを探す列で、あるレコードに当たって更新に回した件数",
                    present_if = "/match_on"
                ),
            ],
            "",
        ),
        N::HumanApproval => Fields(
            "",
            &[
                field!("approval_id", "string"),
                field!(
                    "decision",
                    "\"approve\" | \"reject\" | \"return\" | \"withdrawn\" | \"timeout\""
                ),
                field!("to", "string | null", "判断で進んだ状態"),
                field!("decided_by", "string | null", "判断した人の ID"),
                field!("decided_at", "string | null"),
                field!("comment", "string | null"),
                field!("record_id", "string"),
                field!("rev", "number | null", "判断の後の rev"),
            ],
            "（行の値は載せない。必要なら後で「データを調べる」で読む）",
        ),
        // 日付は YYYY-MM-DD（テーブルの日付の列と比べられる）、日時は時差つき（日時の列と比べられる）・GAP-15。
        N::TransformDate => Fields(
            "",
            &[
                field!("date", "string", "ずらした日・YYYY-MM-DD"),
                field!("start", "string", "期間の最初の日（週の初め・月初など）"),
                field!("end", "string", "期間の最後の日（週の終わり・月末など）"),
                field!("start_at", "string", "期間の始まりの日時・時差つき"),
                field!(
                    "next_start_at",
                    "string",
                    "次の期間の始まりの日時・期間はこれより前まで"
                ),
                field!("year", "number", "ずらした日の年"),
                field!("month", "number", "ずらした日の月"),
                field!("day", "number", "ずらした日の日"),
                field!("weekday", "number", "ずらした日の曜日・1＝月曜〜7＝日曜"),
            ],
            "",
        ),
        N::NotifySend
        | N::ControlLoop
        | N::ControlAssert
        | N::DebugLog
        | N::TransformTemplate
        | N::TransformParse
        | N::TransformSerialize
        | N::TransformRegex
        | N::TransformMap
        | N::TransformFilter
        | N::TransformReduce
        | N::SheetRead
        | N::SheetWrite
        | N::SheetAppend
        | N::DocRead
        | N::DocEdit
        | N::DocComment
        | N::MemoryGet
        | N::MemorySet
        | N::EventPublish
        | N::WorkflowCall
        | N::LlmEmbed
        | N::LlmExtract
        | N::AiReview
        | N::AiEval
        | N::AiOcr
        | N::AiImageAnalyze
        | N::AiImageGenerate
        | N::AiTranscribe
        | N::AiSpeech
        | N::GraphqlQuery
        | N::SandboxExec => Prose(""),
    }
}
