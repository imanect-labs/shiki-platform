//! ノードカタログの単一定義（UI パレット・右パネル・AI ツール description の共通ソース）。
//!
//! 日本語ラベル・カテゴリ・出力ポート・要求スコープ・冪等性区分・timeout 既定を
//! **Rust 側の単一定義**から供給する（ir.md §7 の表の実体化・codegen が正）。
//! `bin/export-ts.rs` が JSON 化して `workflow-catalog.ts` に書き出し、AI 編集ツールの
//! description（Task 10.13）も同じ関数から組み立てる。
//!
//! 表示規約: ラベル/説明は IT に詳しくない利用者向けの日本語（専門語を避ける）。
//! `available == false` は予約語彙（UI は「近日対応」でグレーアウト・V3 が保存拒否）。

use serde::Serialize;
use ts_rs::TS;

use super::{required_scope, NodeType, Scope};

/// パレットのカテゴリ（表示順に定義）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum NodeCategory {
    /// 流れの制御（分岐・繰り返し・待機）。
    Control,
    /// ドライブのファイル操作。
    Storage,
    /// AI（生成・エージェント・検索）。
    Ai,
    /// 外部サービス連携。
    External,
    /// 開発者向け（スクリプト・デバッグ）。
    Developer,
    /// フロー間連携。
    Workflow,
    /// 業務データ（テーブル・Task 15.7）。
    Data,
    /// 通知・人間参加（予約）。
    Notify,
    /// スキル（予約）。
    Skill,
    /// データ加工（予約）。
    Transform,
    /// オフィス文書（予約）。
    Office,
    /// 記憶・状態（予約）。
    Memory,
}

impl NodeCategory {
    /// カテゴリの日本語見出し。
    #[must_use]
    pub const fn label_ja(self) -> &'static str {
        match self {
            NodeCategory::Control => "流れの制御",
            NodeCategory::Storage => "ファイル",
            NodeCategory::Ai => "AI",
            NodeCategory::External => "外部連携",
            NodeCategory::Developer => "開発者向け",
            NodeCategory::Workflow => "フロー連携",
            NodeCategory::Data => "業務データ",
            NodeCategory::Notify => "通知・承認",
            NodeCategory::Skill => "スキル",
            NodeCategory::Transform => "データ加工",
            NodeCategory::Office => "オフィス文書",
            NodeCategory::Memory => "記憶",
        }
    }

    /// 表示順の全カテゴリ。
    pub const ALL: &'static [NodeCategory] = &[
        NodeCategory::Control,
        NodeCategory::Storage,
        NodeCategory::Ai,
        NodeCategory::External,
        NodeCategory::Workflow,
        NodeCategory::Developer,
        NodeCategory::Data,
        NodeCategory::Notify,
        NodeCategory::Skill,
        NodeCategory::Transform,
        NodeCategory::Office,
        NodeCategory::Memory,
    ];
}

/// 冪等性区分（ir.md §7・UI が「再実行で二重になり得るか」を表示する）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum IdempotencyClass {
    /// 副作用なし。
    Pure,
    /// 内部チョークポイントが冪等キーで高々 1 回に潰す。
    EngineDedup,
    /// 外部副作用（exactly-once を約束しない・UI に明示）。
    BestEffort,
}

/// カタログの 1 エントリ（1 ノード種）。
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct NodeCatalogEntry {
    /// ノード種（IR の `type` 値）。
    #[serde(rename = "type")]
    #[ts(type = "NodeType", rename = "type")]
    pub node_type: NodeType,
    /// パレットのカテゴリ。
    pub category: NodeCategory,
    /// カテゴリの日本語見出し（表示用・category から導出）。
    pub category_label_ja: &'static str,
    /// ノードの日本語ラベル。
    pub label_ja: &'static str,
    /// 1 行説明（IT に詳しくない利用者向け）。
    pub description_ja: &'static str,
    /// 静的な出力ポート（switch は cases から動的導出のため空＋`dynamic_ports`）。
    /// `on_error=continue` の `error`・wait(event, on_timeout=continue) の `timeout` は
    /// 設定から導出する構造ポート（ir.md §5.2）でありここに含まない。
    pub output_ports: &'static [&'static str],
    /// 出力ポートが params から動的に決まるか（control.switch のみ）。
    pub dynamic_ports: bool,
    /// 入エッジを複数受けられるか（control.join のみ・V2 の入エッジ 1 本制約の例外）。
    pub multi_input: bool,
    /// 要求スコープ（declared_scopes への宣言が必要・None は scope 天井対象外）。
    #[ts(type = "Scope | null")]
    pub required_scope: Option<Scope>,
    /// 現ステージで保存・実行できるか（false は予約語彙＝UI は「近日対応」）。
    pub available: bool,
    /// 冪等性区分。
    pub idempotency: IdempotencyClass,
    /// step timeout の既定秒（ir.md §7 の表・未規定の予約語彙と制御ノードは None）。
    pub timeout_default_sec: Option<u32>,
    /// step timeout の上限秒。
    pub timeout_max_sec: Option<u32>,
    /// 出力の形（TS 風の表記・後続が `$from: nodes.<id>.output` の path で参照する）。予約語彙は空文字。
    pub output_shape: &'static str,
}

/// カタログ全量（NodeType::ALL と同順・exhaustive match で漏れを構造的に防ぐ）。
#[must_use]
pub fn node_catalog() -> Vec<NodeCatalogEntry> {
    NodeType::ALL.iter().map(|nt| entry(*nt)).collect()
}

/// 1 ノード種のエントリを組む。
/// カテゴリ・日本語ラベル・説明の表（ノード種ごと・exhaustive・データ表のため行数 lint 対象外）。
#[allow(clippy::too_many_lines)]
fn labels(nt: NodeType) -> (NodeCategory, &'static str, &'static str) {
    use NodeCategory as C;
    match nt {
        NodeType::ControlBranch => (
            C::Control,
            "条件分岐",
            "条件を満たすかどうかで「はい / いいえ」に分ける",
        ),
        NodeType::ControlSwitch => (C::Control, "振り分け", "値に応じて複数の行き先に振り分ける"),
        NodeType::ControlJoin => (C::Control, "合流", "分かれた流れを待ち合わせて 1 つに戻す"),
        NodeType::ControlMap => (
            C::Control,
            "繰り返し",
            "リストの要素ごとに同じ処理を繰り返す",
        ),
        NodeType::ControlWait => (C::Control, "待機", "指定した時間・時刻・できごとまで待つ"),
        NodeType::StorageRead => (
            C::Storage,
            "ファイルを読む",
            "ドライブのファイルの内容を取得する",
        ),
        NodeType::StorageWrite => (
            C::Storage,
            "ファイルを保存",
            "ドライブに新しいファイルを保存する",
        ),
        NodeType::StorageList => (
            C::Storage,
            "フォルダ一覧",
            "フォルダの中のファイル一覧を取得する",
        ),
        NodeType::RagSearch => (C::Ai, "社内検索", "権限の範囲で社内ドキュメントを検索する"),
        NodeType::LlmInvoke => (C::Ai, "AI に聞く", "AI に文章の生成・要約・分類などを頼む"),
        NodeType::AgentInvoke => (
            C::Ai,
            "AI エージェント",
            "AI がツールを使って複数手順の作業をこなす",
        ),
        NodeType::HttpRequest => (
            C::External,
            "外部 API 呼び出し",
            "社外のサービス（API）にリクエストを送る",
        ),
        NodeType::ScriptRun => (
            C::Developer,
            "スクリプト",
            "TypeScript の小さなプログラムで計算・変換する",
        ),
        NodeType::WorkflowStart => (
            C::Workflow,
            "別のフローを開始",
            "別のワークフローを起動する（結果は待たない）",
        ),
        NodeType::WorkflowReply => (
            C::Workflow,
            "結果を画面に返す",
            "ボタンや会話から起動した人の画面に、処理の結果を表示する",
        ),
        NodeType::CsvQuery => (
            C::Storage,
            "CSV を集計",
            "CSV ファイルに読み取り専用の SQL を実行して集計・抽出する",
        ),
        NodeType::CsvPatch => (
            C::Storage,
            "CSV を編集",
            "CSV のセル・行・列を書き換えて新しいバージョンを保存する",
        ),
        NodeType::CsvWrite => (
            C::Storage,
            "CSV を保存",
            "新しい CSV ファイルをドライブに保存する",
        ),
        NodeType::DataQuery => (
            C::Data,
            "データを調べる",
            "業務データ（テーブル）を検索・集計する",
        ),
        NodeType::DataRecordCreate => (
            C::Data,
            "データを追加",
            "業務データに新しいレコードを追加する",
        ),
        NodeType::DataRecordUpdate => (C::Data, "データを更新", "業務データのレコードを書き換える"),
        NodeType::DataTransition => (C::Data, "状態を進める", "申請・承認などの状態を次へ進める"),
        NodeType::DataRecordDelete => (C::Data, "データを削除", "業務データのレコードを削除する"),
        NodeType::DataBulkUpsert => (
            C::Data,
            "データをまとめて登録",
            "業務データにレコードをまとめて追加・更新する",
        ),
        NodeType::NotifySend => (C::Notify, "通知を送る", "アプリ内の通知を相手に届ける"),
        NodeType::SkillInvoke => (
            C::Skill,
            "スキルを使う",
            "インストール済みのスキル（定型作業）を呼び出す",
        ),
        NodeType::ControlLoop => (C::Control, "条件ループ", "条件を満たす間、処理を繰り返す"),
        NodeType::ControlAssert => (
            C::Control,
            "チェック",
            "条件を満たさなければフローを失敗として止める",
        ),
        NodeType::DebugLog => (C::Developer, "ログ出力", "実行履歴にメモ（ログ）を残す"),
        NodeType::TransformTemplate => (
            C::Transform,
            "文章の組み立て",
            "テンプレートに値を差し込んで文章を作る",
        ),
        NodeType::TransformParse => (
            C::Transform,
            "データの読み取り",
            "CSV・XML・JSON を扱えるデータに変換する",
        ),
        NodeType::TransformSerialize => (
            C::Transform,
            "データの書き出し",
            "データを CSV・XML・JSON の形式にする",
        ),
        NodeType::TransformRegex => (
            C::Transform,
            "パターン抽出",
            "文字列から規則（正規表現）で抜き出す",
        ),
        NodeType::TransformMap => (
            C::Transform,
            "一括変換",
            "リストの各要素を同じ規則で変換する",
        ),
        NodeType::TransformFilter => (C::Transform, "絞り込み", "リストから条件に合う要素だけ残す"),
        NodeType::TransformReduce => (C::Transform, "集計", "リストを合計・件数などにまとめる"),
        NodeType::SheetRead => (
            C::Office,
            "シートを読む",
            "スプレッドシートのセル・範囲を読み取る",
        ),
        NodeType::SheetWrite => (
            C::Office,
            "シートに書く",
            "スプレッドシートのセル・範囲に書き込む",
        ),
        NodeType::SheetAppend => (
            C::Office,
            "シートに追記",
            "スプレッドシートの末尾に行を追加する",
        ),
        NodeType::DocRead => (
            C::Office,
            "文書を読む",
            "ドキュメントの見出し・本文を読み取る",
        ),
        NodeType::DocEdit => (C::Office, "文書を編集", "ドキュメントの内容を書き換える"),
        NodeType::DocComment => (
            C::Office,
            "文書にコメント",
            "ドキュメントにコメントを付ける",
        ),
        NodeType::MemoryGet => (C::Memory, "記憶を読む", "保存しておいた値を取り出す"),
        NodeType::MemorySet => (C::Memory, "記憶する", "後で使う値を保存しておく"),
        NodeType::EventPublish => (
            C::Workflow,
            "できごとを知らせる",
            "他のフローが購読できるできごとを発行する",
        ),
        NodeType::WorkflowCall => (
            C::Workflow,
            "別のフローを呼ぶ",
            "別のワークフローを実行し結果を受け取る",
        ),
        NodeType::LlmEmbed => (C::Ai, "AI 埋め込み", "検索用の数値表現（埋め込み）を作る"),
        NodeType::LlmExtract => (C::Ai, "AI で抽出", "文章から決まった形式でデータを抜き出す"),
        NodeType::AiReview => (
            C::Ai,
            "AI レビュー",
            "文章やコードを AI がレビューして指摘を返す",
        ),
        NodeType::AiEval => (C::Ai, "AI 品質評価", "AI の出力の品質を AI が採点する"),
        NodeType::AiOcr => (C::Ai, "文字起こし（画像）", "画像や PDF から文字を読み取る"),
        NodeType::AiImageAnalyze => (C::Ai, "画像の理解", "画像の内容を AI が説明・分類する"),
        NodeType::AiImageGenerate => (C::Ai, "画像の生成", "指示から画像を生成する"),
        NodeType::AiTranscribe => (C::Ai, "文字起こし（音声）", "音声を文字に起こす"),
        NodeType::AiSpeech => (C::Ai, "音声の生成", "文章を読み上げ音声にする"),
        NodeType::GraphqlQuery => (
            C::External,
            "GraphQL 呼び出し",
            "GraphQL の外部サービスに問い合わせる",
        ),
        NodeType::SandboxExec => (
            C::Developer,
            "コマンド実行",
            "隔離環境でコマンドや Python を 1 回実行する",
        ),
        NodeType::HumanApproval => (
            C::Notify,
            "承認を待つ",
            "テーブルの行について担当者の承認・却下・差し戻しを待って分岐する",
        ),
    }
}

/// 静的な出力ポートと、ポートが params から動的に決まるか（ir.md §5.2）。
///
/// switch のみ動的・branch は true/false・approval は approved / withdrawn（rejected・returned・timeout は
/// 設定から足す構造ポート・[`approval_ports`]）。カタログと保存時検証 V2（`from_port` の存在）が読む単一定義。
pub(crate) fn static_output_ports(nt: NodeType) -> (&'static [&'static str], bool) {
    match nt {
        NodeType::ControlBranch => (&["true", "false"], false),
        NodeType::ControlSwitch => (&[], true),
        NodeType::HumanApproval => (&["approved", "withdrawn"], false),
        _ => (&["out"], false),
    }
}

/// human.approval の出口（承認・却下・差し戻し・取り下げ・時間切れ）を params から導く（GAP-08）。
///
/// `rejected` は reject を、`returned` は return を指定したとき、`timeout` は on_timeout=continue のときだけ。
/// V2（ポートの存在）・executor（起床で取る出口）・エディタ（ハンドル）が同じ規則を使う。
#[must_use]
pub fn approval_ports(params: &serde_json::Value) -> Vec<&'static str> {
    let has = |k: &str| params.get(k).is_some_and(|v| !v.is_null());
    let mut ports = vec!["approved"];
    if has("reject") {
        ports.push("rejected");
    }
    if has("return") {
        ports.push("returned");
    }
    ports.push("withdrawn");
    if params.get("on_timeout").and_then(serde_json::Value::as_str) == Some("continue") {
        ports.push("timeout");
    }
    ports
}

/// 1 ノード種のエントリを組む。
fn entry(nt: NodeType) -> NodeCatalogEntry {
    use IdempotencyClass::{BestEffort, EngineDedup, Pure};

    let (category, label_ja, description_ja) = labels(nt);

    let (output_ports, dynamic_ports) = static_output_ports(nt);

    // 冪等性区分（ir.md §7 の表・予約語彙は設計上の区分）。
    let idempotency = match nt {
        NodeType::StorageWrite
        | NodeType::DataRecordCreate
        | NodeType::DataRecordUpdate
        | NodeType::DataTransition
        | NodeType::DataRecordDelete
        | NodeType::DataBulkUpsert
        | NodeType::NotifySend
        | NodeType::WorkflowStart
        | NodeType::MemorySet
        | NodeType::EventPublish
        | NodeType::CsvPatch
        | NodeType::CsvWrite
        | NodeType::WorkflowCall
        | NodeType::HumanApproval => EngineDedup,
        NodeType::ScriptRun
        | NodeType::AgentInvoke
        | NodeType::HttpRequest
        | NodeType::GraphqlQuery
        | NodeType::SandboxExec
        | NodeType::SkillInvoke
        | NodeType::SheetWrite
        | NodeType::SheetAppend
        | NodeType::DocEdit
        | NodeType::DocComment
        | NodeType::AiImageGenerate
        | NodeType::AiSpeech => BestEffort,
        _ => Pure,
    };

    // timeout 既定/上限（ir.md §7 の表が規定する種のみ・制御と未規定の予約は None）。
    let (timeout_default_sec, timeout_max_sec) = match nt {
        NodeType::StorageRead
        | NodeType::StorageWrite
        | NodeType::StorageList
        | NodeType::CsvQuery
        | NodeType::CsvPatch
        | NodeType::CsvWrite
        | NodeType::DataQuery
        | NodeType::DataRecordCreate
        | NodeType::DataRecordUpdate
        | NodeType::DataRecordDelete
        | NodeType::DataTransition
        | NodeType::HttpRequest
        // 承認の step は行を読んで止まるまで（判断を待つ時間は timeout_sec・run_timeout_sec が決める）。
        | NodeType::HumanApproval => (Some(30), Some(120)),
        NodeType::RagSearch => (Some(60), Some(120)),
        NodeType::NotifySend => (Some(30), Some(60)),
        NodeType::WorkflowStart => (Some(10), Some(30)),
        // 値を整形するだけ（能力を呼ばない）。
        NodeType::WorkflowReply => (Some(5), Some(10)),
        NodeType::ScriptRun => (Some(30), Some(360)),
        NodeType::SkillInvoke => (Some(60), Some(360)),
        // 一括 upsert は取込ジョブを同期で最後まで進める（最大 5,000 行・1 チャンク 500 行）。
        NodeType::LlmInvoke | NodeType::DataBulkUpsert => (Some(120), Some(600)),
        NodeType::AgentInvoke => (Some(600), Some(3600)),
        _ => (None, None),
    };

    NodeCatalogEntry {
        node_type: nt,
        category,
        category_label_ja: category.label_ja(),
        label_ja,
        description_ja,
        output_ports,
        dynamic_ports,
        multi_input: nt == NodeType::ControlJoin,
        required_scope: required_scope(nt),
        available: nt.available_stage_a(),
        idempotency,
        timeout_default_sec,
        timeout_max_sec,
        output_shape: output_shape(nt),
    }
}

/// 出力の形（ノード種ごと・exhaustive）。AI の説明文と右パネルの「前のノードの出力」の単一定義。
///
/// 出力は能力ポート（api の `ProdNodePorts`・storage・data）と executor が組む JSON で、型としての
/// 定義を持たない。ここを変えずに出力を変えると AI が誤った path を書くので、出力側を変えたら
/// ここも直す。予約語彙は空文字（実装時に足す）。
#[allow(clippy::too_many_lines)]
const fn output_shape(nt: NodeType) -> &'static str {
    use NodeType as N;
    match nt {
        N::ControlBranch | N::ControlSwitch | N::ControlJoin => {
            "実行の入力をそのまま通す（分岐・合流の結果としては参照しない）"
        }
        N::ControlMap => {
            "{ items: Array<unknown>（領域の出口ノードの出力・要素の順）, errors: Array<{ index: number, error: { code: string, message: string } }>（on_item_error: collect のとき） }"
        }
        N::ControlWait => "event なら届いたできごとの内容・時間待ちは null",
        N::StorageRead => {
            "{ id: string, name: string, content_type: string, size: number, text: string }"
        }
        N::StorageWrite => {
            "{ id: string, name: string, version: number, sha256: string, size: number }"
        }
        N::StorageList => {
            "{ items: Array<{ id: string, name: string, kind: string, size: number }> }（最大 100 件）"
        }
        N::RagSearch => {
            "{ results: Array<{ file_id: string, file_name: string, content: string, score: number }> }"
        }
        N::LlmInvoke => "{ text: string, model: string }",
        N::AgentInvoke => "{ stdout: string, exit_code: number }",
        N::HttpRequest => {
            "{ status: number, host: string, body: unknown（JSON なら解析した値・それ以外は文字列） }"
        }
        N::ScriptRun => "main(input) の戻り値そのもの",
        N::WorkflowStart => "{ run_id: string | null }（子の結果は待たない）",
        N::WorkflowReply => {
            "画面に出す節 { format: \"text\" | \"markdown\" | \"table\" | \"file_link\", label?: string, tone: string, truncated: boolean, text?: string（text・markdown）, columns?: Array<string>, rows?: Array<Array<string>>, total_rows?: number（table）, files?: Array<{ id: string, name: string }>（file_link） }（後続から参照するものではない）"
        }
        N::SkillInvoke => {
            "script を持つ skill は main の戻り値・指示だけの skill は { text: string, model: string }"
        }
        N::CsvQuery => {
            "{ columns: Array<string>, column_types: Array<string>, rows: Array<Array<unknown>>, total_rows: number, truncated: boolean }"
        }
        N::CsvPatch => "{ node_id: string, version: number, rows: number, cols: number }",
        N::CsvWrite => "{ node_id: string, version: number, name: string }",
        N::DataQuery => {
            "{ items?: Array<{ id: string, data: { [列の参照名]: unknown }, rev: number, … }>, groups?: Array<集計の行>（aggregate のとき）, next_cursor?: string, count?: { exact: number } | { at_least: number }（with_count のとき） }"
        }
        N::DataRecordCreate | N::DataRecordUpdate | N::DataTransition => {
            "レコード { id: string, table_id: string, data: { [列の参照名]: unknown }, rev: number, owner: string, created_at: string, updated_at: string }"
        }
        N::DataRecordDelete => "{ id: string, deleted: true }",
        N::DataBulkUpsert => {
            "{ import_id: string, status: string, rows_total: number, rows_applied: number, rows_invalid: number, rows_rejected: number, errors: Array<{ row: number, key: string | null, message: string }>, last_error: string | null }"
        }
        N::HumanApproval => {
            "{ approval_id: string, decision: \"approve\" | \"reject\" | \"return\" | \"withdrawn\" | \"timeout\", to: string | null（判断で進んだ状態）, decided_by: string | null（判断した人の ID）, decided_at: string | null, comment: string | null, record_id: string, rev: number | null（判断の後の rev） }（行の値は載せない。必要なら後で「データを調べる」で読む）"
        }
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
        | N::SandboxExec => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_covers_all_vocab_in_order() {
        let catalog = node_catalog();
        assert_eq!(catalog.len(), NodeType::ALL.len());
        for (entry, nt) in catalog.iter().zip(NodeType::ALL) {
            assert_eq!(entry.node_type, *nt);
        }
    }

    #[test]
    fn stage_a_entries_have_timeout_or_are_control() {
        for e in node_catalog().iter().filter(|e| e.available) {
            assert!(
                e.node_type.is_control() || e.timeout_default_sec.is_some(),
                "{} は timeout 既定が必要",
                e.node_type.as_str()
            );
        }
    }

    #[test]
    fn structural_flags_match_vocab() {
        let catalog = node_catalog();
        let find = |nt: NodeType| catalog.iter().find(|e| e.node_type == nt).unwrap();
        assert!(find(NodeType::ControlJoin).multi_input);
        assert!(find(NodeType::ControlSwitch).dynamic_ports);
        assert_eq!(
            find(NodeType::ControlBranch).output_ports,
            ["true", "false"]
        );
        assert_eq!(
            find(NodeType::StorageWrite).required_scope,
            Some(Scope::StorageWrite)
        );
        assert_eq!(
            find(NodeType::StorageWrite).idempotency,
            IdempotencyClass::EngineDedup
        );
    }

    #[test]
    fn labels_are_nonempty_and_unique_within_category() {
        use std::collections::BTreeSet;
        let mut seen: BTreeSet<(String, &str)> = BTreeSet::new();
        for e in node_catalog() {
            assert!(!e.label_ja.is_empty());
            assert!(!e.description_ja.is_empty());
            assert!(
                seen.insert((format!("{:?}", e.category), e.label_ja)),
                "カテゴリ内でラベル重複: {}",
                e.label_ja
            );
        }
    }

    /// 使えるノードは出力の形を持ち（AI の説明文・右パネルが参照する）、予約語彙は持たない。
    #[test]
    fn output_shape_is_defined_exactly_for_stage_a_nodes() {
        for e in node_catalog() {
            assert_eq!(
                !e.output_shape.is_empty(),
                e.available,
                "{} の出力の形",
                e.node_type.as_str()
            );
        }
        let find = |nt: NodeType| {
            node_catalog()
                .into_iter()
                .find(|e| e.node_type == nt)
                .unwrap()
        };
        assert_eq!(
            find(NodeType::LlmInvoke).output_shape,
            "{ text: string, model: string }"
        );
    }

    #[test]
    fn catalog_serializes_to_json() {
        let json = serde_json::to_value(node_catalog()).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr[0]["type"], "control.branch");
        assert_eq!(arr[0]["category_label_ja"], "流れの制御");
    }
}
