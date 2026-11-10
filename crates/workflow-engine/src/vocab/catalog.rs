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

mod output_shape;
use output_shape::output_shape;

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
    /// 静的な出力ポート（switch・ai.decide は params から導出するため空＋`dynamic_ports`）。
    /// `on_error=continue` の `error`・wait(event, on_timeout=continue) の `timeout` は
    /// 設定から導出する構造ポート（ir.md §5.2）でありここに含まない。
    pub output_ports: &'static [&'static str],
    /// 出力ポートが params から動的に決まるか（control.switch・ai.decide。互換のため残す。どう決まるかは
    /// [`Self::port_rule`]）。
    pub dynamic_ports: bool,
    /// 出力ポートの決まり方（[`PortRule`]・保存時検証 V2 と検証 API の `node_ports` が同じ規則で導く）。
    pub port_rule: PortRule,
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
    /// step 属性 `cache` を付けられるか（付けられる種はその期限の決まり・いまは ai.decide の決定キャッシュだけ）。
    pub step_cache: Option<crate::ir::StepCacheLimits>,
    /// 出力の形（TS 風の表記・人と AI が読む）。[`Self::output_fields`] から作る（項目に分けられない形は文）。
    /// 予約語彙は空文字。
    pub output_shape: String,
    /// 出力の最上位の項目（後続が `$from: nodes.<id>.output` の `/<name>` で読む・右パネルの参照の候補）。
    /// 項目の組でない出力（入力をそのまま通す・script の戻り値など）と予約語彙は空（D-42）。
    pub output_fields: &'static [OutputField],
}

/// 出力ポートの決まり方（ir.md §5.2・カタログの単一定義）。
///
/// 実際のポート集合は `ir::validate::node_ports` が params からこの規則で導く（V2 のポート存在検査と検証
/// API の `node_ports` が同じ関数を使い、ai.decide の実行時の振り分けも同じ規則（`AiDecideParams::ports`）
/// に従う。web に導出を書き写さない）。どの規則でも `on_error: continue` なら `error`、
/// wait(event, on_timeout=continue) なら `timeout` が加わる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PortRule {
    /// 静的なポート（[`NodeCatalogEntry::output_ports`]）だけ。
    Static,
    /// control.switch: `cases[].port` と `default`（`out` は発しない）。
    SwitchCases,
    /// human.approval: `approved`・`withdrawn` に、指定した判断（`rejected`・`returned`）と時間切れ（`timeout`）。
    ApprovalChoices,
    /// ai.decide: 振り分け（`route`）から決まる（無し＝`out`・確からしさで＝`out`/`uncertain`・
    /// 値で＝選択肢のキーか `true`/`false` と `uncertain`）。
    DecideRoute,
}

/// 出力の最上位の 1 項目（カタログの単一定義・D-42）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub struct OutputField {
    /// 項目名（path は `/<name>`）。
    pub name: &'static str,
    /// 型（TS 風の表記・入れ子の形も含む）。
    #[serde(rename = "type")]
    #[ts(rename = "type")]
    pub ty: &'static str,
    /// 無いことがある（表記は `name?:`）。
    pub optional: bool,
    /// 補足（いつ入るか・何が入るか。空なら無し・表記は型の後ろの（…））。
    pub note: &'static str,
    /// この項目が入るのは、ブロックの設定（params）のこの場所（JSON Pointer）に値があるときだけ（空なら、設定に
    /// かかわらず入る）。値が無い・`false`・空の文字・空のリスト・空のオブジェクトは「値が無い」。画面は値の候補を
    /// これで出し分ける（選んでも値が無く実行で失敗する候補を出さない・BUG-108）。
    pub present_if: &'static str,
}

impl OutputField {
    /// この項目が、ブロックの設定（`params`）のもとで出力に入るか（[`Self::present_if`] の約束・GAP-112）。
    ///
    /// `Some(true)` は入る（`present_if` が空なら設定にかかわらず入る）、`Some(false)` は入らない（場所に値が無い・
    /// `null`・`false`・空の文字・空のリスト・空のオブジェクト）。`None` は設定からは決まらない（場所か、そこへ至る
    /// 途中に `$from`・`$template` の値式があり、実行の値で決まる）。画面の値の候補（web の `value-refs.ts` の
    /// `hasParamAt`）と同じ約束で、保存時の照合（`ir.unknown_output_path`）と評価の ir_lint が使う。
    #[must_use]
    pub fn present_in(&self, params: &serde_json::Value) -> Option<bool> {
        use serde_json::Value;
        let at = self.present_if;
        if at.is_empty() {
            return Some(true);
        }
        let is_expr = |v: &Value| {
            v.as_object()
                .is_some_and(|m| m.contains_key("$from") || m.contains_key("$template"))
        };
        // 根から場所まで、JSON Pointer の接頭辞ごとの値（途中が値式なら、その先は実行の値で決まる）。
        let prefixes = std::iter::once("")
            .chain(at.match_indices('/').skip(1).map(|(i, _)| &at[..i]))
            .chain(std::iter::once(at));
        let mut last = params;
        for p in prefixes {
            match params.pointer(p) {
                None => return Some(false),
                Some(v) if is_expr(v) => return None,
                Some(v) => last = v,
            }
        }
        Some(match last {
            Value::Null | Value::Bool(false) => false,
            Value::String(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(m) => !m.is_empty(),
            Value::Bool(true) | Value::Number(_) => true,
        })
    }
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
            // 文章として読めないファイルを文字化けのまま読まない（BUG-101）。PDF・Office は社内文書の検索に取り込まれた
            // 本文を読む（D-111・付録 D の N3.5.1）。
            "ファイルの中身を文章として読みます。PDF や Word は、社内文書の検索に取り込まれた本文を読みます。",
        ),
        NodeType::StorageWrite => (
            C::Storage,
            "ファイルを保存",
            // 上書きも新しいバージョンもしない（同じ名前の扱いは設定で決める・BUG-97・付録 D の N3.5.2）。
            "ドライブに新しいファイルを作って保存します。すでにあるファイルは書き換えません。",
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
        NodeType::AiDecide => (
            C::Ai,
            "AI で判定",
            "質問に選択肢・段階・はい/いいえで答え、確からしさを付けて返す",
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
            "CSV か Excel のファイルを SQL で集計したり、条件に合う行を取り出したりします。複数の CSV をつないで集計することもできます。ファイルは書き換えません。",
        ),
        NodeType::CsvPatch => (
            C::Storage,
            "CSV を編集",
            "CSV のセル・行・列を書き換えて新しいバージョンを保存する",
        ),
        NodeType::CsvWrite => (
            C::Storage,
            "CSV を保存",
            "ドライブに新しい CSV ファイルを作って保存します。すでにあるファイルは書き換えません。",
        ),
        NodeType::DataQuery => (
            C::Data,
            "データを調べる",
            // 集計は画面では作れない（AI 向けには params の形の説明で補う・BUG-108・付録 D の N3.3.1）。
            "テーブルから、条件に合うレコードを読みます。一度に読めるのは最大 200 件です。",
        ),
        NodeType::DataRecordCreate => (
            C::Data,
            "データを追加",
            "業務データに新しいレコードを追加する",
        ),
        NodeType::DataRecordUpdate => (C::Data, "データを更新", "業務データのレコードを書き換える"),
        // 名前と説明は付録 D の新しい呼び名（レコードを〜）にそろえる（ほかのブロックは FEAT-3 の書き直しで合わせる）。
        NodeType::DataRecordUpsert => (
            C::Data,
            "レコードを追加か更新",
            "選んだ列の値が同じレコードがあれば書き換え、なければ追加します。",
        ),
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
        // 前日・先週・先月・月末を script なしで作る（GAP-15）。
        NodeType::TransformDate => (
            C::Transform,
            "日付を計算",
            "基準の日から、前日・先週・先月などの日付と、週や月の最初と最後の日を求めます。",
        ),
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

/// 静的な出力ポートと、ポートの決まり方（ir.md §5.2）。
///
/// branch は true/false・switch は cases から・approval は approved / withdrawn に指定した判断を足す
/// （[`approval_ports`]）・ai.decide は振り分けから（`AiDecideParams::ports`）。カタログと保存時検証 V2
/// （`from_port` の存在）が読む単一定義。
pub(crate) fn output_port_rule(nt: NodeType) -> (&'static [&'static str], PortRule) {
    match nt {
        NodeType::ControlBranch => (&["true", "false"], PortRule::Static),
        NodeType::ControlSwitch => (&[], PortRule::SwitchCases),
        NodeType::HumanApproval => (&["approved", "withdrawn"], PortRule::ApprovalChoices),
        NodeType::AiDecide => (&[], PortRule::DecideRoute),
        _ => (&["out"], PortRule::Static),
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

/// ノード種の日本語の名前（画面のブロックの名前・誤りの文言でブロックを名指しする）。
pub(crate) fn label_ja(nt: NodeType) -> &'static str {
    labels(nt).1
}

/// 1 ノード種のエントリを組む。
fn entry(nt: NodeType) -> NodeCatalogEntry {
    use IdempotencyClass::{BestEffort, EngineDedup, Pure};

    let (category, label_ja, description_ja) = labels(nt);

    let (output_ports, port_rule) = output_port_rule(nt);
    // 互換の印: params から決まるのは switch と ai.decide（承認は静的な出口に足すだけなので従来どおり false）。
    let dynamic_ports = matches!(port_rule, PortRule::SwitchCases | PortRule::DecideRoute);

    // 冪等性区分（ir.md §7 の表・予約語彙は設計上の区分）。
    let idempotency = match nt {
        NodeType::StorageWrite
        | NodeType::DataRecordCreate
        | NodeType::DataRecordUpdate
        | NodeType::DataRecordUpsert
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

    let (timeout_default_sec, timeout_max_sec) = step_timeout_limits(nt);

    let shape = output_shape(nt);
    NodeCatalogEntry {
        node_type: nt,
        category,
        category_label_ja: category.label_ja(),
        label_ja,
        description_ja,
        output_ports,
        dynamic_ports,
        port_rule,
        multi_input: nt == NodeType::ControlJoin,
        required_scope: required_scope(nt),
        available: nt.available_stage_a(),
        idempotency,
        timeout_default_sec,
        timeout_max_sec,
        step_cache: step_cache(nt),
        output_shape: shape.render(),
        output_fields: shape.fields(),
    }
}

/// step の時間の上限の既定と最大（秒・ir.md §7 の表が規定する種のみ・制御と未規定の予約は None）。
///
/// カタログの `timeout_default_sec`・`timeout_max_sec` と、実行の打ち切り（`run::step_timeout`）・保存時の照合（V7）が
/// この 1 つの表を読む（BUG-96）。
#[must_use]
pub fn step_timeout_limits(nt: NodeType) -> (Option<u32>, Option<u32>) {
    match nt {
        NodeType::StorageRead
        | NodeType::StorageWrite
        | NodeType::StorageList
        | NodeType::CsvQuery
        | NodeType::CsvPatch
        | NodeType::CsvWrite
        | NodeType::DataQuery
        | NodeType::DataRecordCreate
        | NodeType::DataRecordUpdate
        | NodeType::DataRecordUpsert
        | NodeType::DataRecordDelete
        | NodeType::DataTransition
        | NodeType::HttpRequest
        // 承認の step は行を読んで止まるまで（判断を待つ時間は timeout_sec・run_timeout_sec が決める）。
        | NodeType::HumanApproval => (Some(30), Some(120)),
        NodeType::RagSearch => (Some(60), Some(120)),
        // 判定は 1 回の要求（多数決は並列）。生成より短いが、思考を伴う LLM の決定器もある（FEAT-1）。
        NodeType::AiDecide => (Some(60), Some(300)),
        NodeType::NotifySend => (Some(30), Some(60)),
        NodeType::WorkflowStart => (Some(10), Some(30)),
        // 値を整形するだけ・日付を計算するだけ（能力を呼ばない）。
        NodeType::WorkflowReply | NodeType::TransformDate => (Some(5), Some(10)),
        NodeType::ScriptRun => (Some(30), Some(360)),
        NodeType::SkillInvoke => (Some(60), Some(360)),
        NodeType::LlmInvoke => (Some(120), Some(600)),
        // 一括 upsert は取込ジョブを同期で最後まで進める（最大 5,000 行・1 チャンク 500 行）。切れると途中まで
        // 登録したまま失敗する（やり直しは続きから）ので、既定でも最大の 600 秒まで待つ（D-106）。
        NodeType::DataBulkUpsert => (Some(600), Some(600)),
        NodeType::AgentInvoke => (Some(600), Some(3600)),
        _ => (None, None),
    }
}

/// step 属性 `cache` の決まり（付けられない種は None・保存時検証が `ir.cache_unsupported` にする）。
#[must_use]
pub fn step_cache(nt: NodeType) -> Option<crate::ir::StepCacheLimits> {
    // 判定の決定キャッシュ（T10・D-52 の H6）。生成（llm.invoke）は答えの形が決まらず、同じ入力でも
    // 別の文章を期待されることがあるので付けない。
    (nt == NodeType::AiDecide).then_some(crate::ir::StepCacheLimits::DECISION)
}

#[cfg(test)]
mod tests;
