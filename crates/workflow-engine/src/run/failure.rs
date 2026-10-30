//! 失敗した run の理由を、利用者向けの分類と文言に置き換える（GAP-09・1 か所で定義する）。
//!
//! step の `error.code` と run の `fail_reason`（run_timeout）から分類を決める。生の `message`（英語の原文・
//! DB の詳細・URL・値を含む競合の詳細など）は返さない。分類は読取のときに決め、終端で書き込まない
//! （再開した run に古い理由を残さない）。
//!
//! 一時障害・時間切れ・分類できない失敗の文言は、押し直しを勧めない。押し直すと新しい冪等キーで別の run に
//! なり、途中まで進んだ副作用が二重になり得る（PIT-31）。

use uuid::Uuid;

use super::outcome::{FailureCategory, ResumeAdvice, ResumeHint, RunFailure};

/// 既知の code と分類（エンジン・api のポート・script の実行時が出すもの）。
///
/// ここに無い code は [`FailureCategory::Unknown`] になる。新しい code を足したら分類も足す（単体テストが
/// ソースを走査して、ここに無い code を見つける）。
pub const KNOWN_CODES: &[(&str, FailureCategory)] = {
    use FailureCategory::{
        Configuration, Conflict, NotFound, Permission, Quota, Temporary, Timeout, Unknown,
    };
    &[
        // 権限
        ("forbidden", Permission),
        ("out_of_scope", Permission),
        // 承認を待つ間に実行主体（委譲・テーブルの権限）が外れた（D-62・`approval::ACTOR_UNAVAILABLE`）。
        (crate::approval::ACTOR_UNAVAILABLE, Permission),
        // 競合
        ("conflict", Conflict),
        ("effect_conflict", Conflict),
        ("unique_violation", Conflict),
        // 保存先に同じ名前のファイルがある（storage.write・csv.write・BUG-97・`nodes::ports::NAME_TAKEN`）。
        (crate::nodes::ports::NAME_TAKEN, Conflict),
        // 見つからない
        ("not_found", NotFound),
        ("gone", NotFound),
        // AI エージェントの道具の失敗（ふつうはモデルへ返り、step の失敗にはならない・BUG-6）。
        ("doc_read_failed", NotFound),
        // 一時障害
        ("unavailable", Temporary),
        ("rate_limited", Temporary),
        ("read_timeout", Temporary),
        ("index_preparing", Temporary),
        // PDF・Office の本文の社内文書の検索への取り込みが終わっていない（storage.read・D-111）。
        (crate::nodes::ports::INDEX_PENDING, Temporary),
        ("effect_in_progress", Temporary),
        ("concurrency_limited", Temporary),
        ("fetch_failed", Temporary),
        // 外部のサービスが一時的な誤り（5xx・429）を返した（http.request の fail_on_error_status・BUG-99）。
        (crate::nodes::http::HTTP_RETRYABLE_STATUS, Temporary),
        // 利用の上限
        ("quota_exceeded", Quota),
        ("sandbox_limit", Quota),
        // AI エージェントが手順・時間・使える量の上限に達しても回答を書けなかった（BUG-6）。
        ("agent_incomplete", Quota),
        // 時間切れ
        ("timeout", Timeout),
        ("run_timeout", Timeout),
        // 承認の期限が切れて run を止めた（on_timeout=fail・BUG-70・`approval::APPROVAL_TIMEOUT`）。
        (crate::approval::APPROVAL_TIMEOUT, Timeout),
        // wait(event) の待つ期限が切れて run を止めた（on_timeout=fail・BUG-112・[`WAIT_TIMEOUT`]）。
        (WAIT_TIMEOUT, Timeout),
        // step を時間の上限で打ち切った（ノードの timeout_sec・retryable・BUG-96）。
        (super::step_timeout::STEP_TIMEOUT, Timeout),
        // 設定の誤り（作成者が直すまで通らない）
        ("invalid", Configuration),
        ("bad_params", Configuration),
        ("bad_request", Configuration),
        ("expr_resolve_error", Configuration),
        ("expr_type_error", Configuration),
        ("field_unavailable", Configuration),
        ("unsupported", Configuration),
        ("unsupported_stage_a", Configuration),
        ("unknown_node", Configuration),
        ("redirect_denied", Configuration),
        // 外部のサービスが誤り（4xx）を返した（送る内容・URL・シークレットを直す・BUG-99）。
        (crate::nodes::http::HTTP_ERROR_STATUS, Configuration),
        ("fanout_limit_exceeded", Configuration),
        // 中にブロックの無い繰り返しを実行しようとした（保存は通り、実行の前に止まるもの・D-85・[`EMPTY_REGION`]）。
        (EMPTY_REGION, Configuration),
        ("model_unavailable", Configuration),
        ("model_choice_disabled", Configuration),
        ("no_model", Configuration),
        // 構造化出力（llm.invoke の output_schema・GAP-13）。プロンプト・出力の形・max_tokens を作成者が見直す。
        (
            crate::nodes::llm_structured::LLM_OUTPUT_INVALID,
            Configuration,
        ),
        (
            crate::nodes::llm_structured::LLM_OUTPUT_TRUNCATED,
            Configuration,
        ),
        (crate::nodes::llm_structured::LLM_REFUSED, Configuration),
        // 判定（ai.decide・FEAT-1）の材料が大きすぎる（作成者が渡す項目を絞る）。
        (
            crate::nodes::decide::DECISION_STATE_TOO_LARGE,
            Configuration,
        ),
        ("approval_not_decidable", Configuration),
        ("approval_no_assignee", Configuration),
        ("approval_assignee_unreadable", Configuration),
        // 文章として読めないファイルを読もうとした（PDF・Office・画像・文字コードを決められない中身・BUG-101・GAP-45）。
        (crate::nodes::ports::NOT_TEXT_FILE, Configuration),
        // 社内文書の検索に取り込めない種類・大きすぎる・本文の無いファイルと、取り込みに失敗したファイルの本文を
        // 読もうとした（storage.read・D-111）。
        (crate::nodes::ports::NOT_INDEXABLE, Configuration),
        (crate::nodes::ports::INDEX_FAILED, Configuration),
        ("script_error", Configuration),
        // AI エージェントが同じ道具の失敗を繰り返した（依頼・道具・読める資料を作成者が見直す・BUG-6）。
        ("agent_loop", Configuration),
        ("frame_violation", Configuration),
        ("resource", Configuration),
        // 分類できない（理由が要素の側にある・内部の失敗）
        ("map_item_failed", Unknown),
        ("item_failed", Unknown),
        ("item_skipped", Unknown),
        ("cancelled", Unknown),
        ("internal", Unknown),
        // 判定の応答が質問と合わない（ポートの実装の誤り・FEAT-1）。
        (crate::nodes::decide::DECISION_INVALID_RESPONSE, Unknown),
    ]
};

/// wait(event) の待つ期限が切れ、`on_timeout = fail` で run を止めたときの理由（BUG-112・step の `error.code` と
/// run の `fail_reason`）。分類は時間切れ。
pub const WAIT_TIMEOUT: &str = "wait_timeout";

/// 中にブロックの無い繰り返し（`control.map`）を実行しようとしたときの step の `error.code`（D-85）。
///
/// 保存は通るが、実行と自動実行のオンの前に止める（`ir.empty_region`）。それをすり抜けた実行も、中の最後の
/// ブロックをいつまでも待たせずに失敗にする（実行時の再検証・ir.md §8）。
pub const EMPTY_REGION: &str = "empty_region";

/// 押し直しを勧めない注記（途中まで進んだ副作用を二重にしない・PIT-31）。
const CHECK_BEFORE_RETRY: &str =
    "途中まで処理が進んでいることがあります。表や結果を確かめてから、必要ならもう一度お試しください。";

/// code の分類（未知の code は Unknown）。
#[must_use]
pub fn category_of(code: &str) -> FailureCategory {
    KNOWN_CODES
        .iter()
        .find(|(c, _)| *c == code)
        .map_or(FailureCategory::Unknown, |(_, cat)| *cat)
}

/// 分類の文言より詳しく言える code の文言（run の `fail_reason` に書く理由を含む・BUG-113）。
///
/// 実行履歴の詳細も画面に返す結果（GAP-09）も、この表から引く（web に対応表を持たない）。
fn specific_message(code: &str) -> Option<&'static str> {
    Some(match code {
        "unique_violation" => "同じ値がすでに登録されているため、登録できませんでした。",
        // 「ほかの操作と重なった…表を読み直して」は原因と合わない（BUG-97・付録 D の N3.5.2）。
        crate::nodes::ports::NAME_TAKEN => {
            "同じ名前のファイルがすでにあるため、保存できませんでした。"
        }
        // 「一時的な問題…途中まで進んでいる」は原因と合わない（読むだけのブロック・D-111）。
        crate::nodes::ports::INDEX_PENDING => {
            "ファイルを社内文書の検索に取り込み終わっていないため、本文を読めませんでした。\
             少し待ってから、失敗したところから再開してください。"
        }
        // 「ワークフローの設定に問題があり」は原因と合わない（ファイルの側の失敗・D-111）。
        crate::nodes::ports::INDEX_FAILED => {
            "ファイルを社内文書の検索に取り込めなかったため、本文を読めませんでした。\
             ファイルを保存し直すと、取り込み直します。"
        }
        // 付録 C の §5.2（failure.rs の時間切れの文言の直し）。
        "run_timeout" => {
            "実行の期限までに終わらなかったため、実行を止めました。途中まで進んでいることがあります。\
             データや結果を確かめてから、必要ならもう一度実行してください。"
        }
        // 付録 D の N3.4.1（承認の期限）。
        crate::approval::APPROVAL_TIMEOUT => {
            "承認の期限までに判断がなかったため、実行を失敗にしました。"
        }
        // 付録 D の N3.1.5（変更を待つ期限）。
        WAIT_TIMEOUT => "待つ期限までに変更が起きなかったため、実行を止めました。",
        // D-85（中にブロックの無い繰り返し）。
        EMPTY_REGION => "繰り返しの中にブロックが無いため、実行を止めました。",

        // 付録 D の N3.4.1（どの出口にも進まないとき）・付録 C の §3.6（実行する人）。
        crate::approval::ACTOR_UNAVAILABLE => {
            "承認を待つあいだに、実行する人（自動実行ではオンにした人）がテーブルを使えなくなったため、\
             実行を止めました。"
        }
        _ => return None,
    })
}

/// 分類の文言（[`specific_message`] にある code はそちら）。
fn message(category: FailureCategory, code: Option<&str>) -> String {
    use FailureCategory as F;
    if let Some(m) = code.and_then(specific_message) {
        return m.to_string();
    }
    match category {
        F::Permission => {
            "権限が足りないため、処理を完了できませんでした。アプリを共有した人に確かめてください。"
                .to_string()
        }
        F::Conflict => "ほかの操作と重なったため、処理を完了できませんでした。表を読み直してから、もう一度お試しください。"
            .to_string(),
        F::NotFound => "対象のデータが見つかりませんでした。".to_string(),
        F::Temporary => format!("一時的な問題で処理を完了できませんでした。{CHECK_BEFORE_RETRY}"),
        F::Quota => "利用の上限に達したため、処理を完了できませんでした。".to_string(),
        F::Timeout => format!("時間内に終わらなかったため、処理を止めました。{CHECK_BEFORE_RETRY}"),
        F::Configuration => {
            "ワークフローの設定に問題があり、処理を完了できませんでした。作成者に連絡してください。"
                .to_string()
        }
        F::Unknown => format!(
            "処理を完了できませんでした。{CHECK_BEFORE_RETRY}問題が続くときは作成者に連絡してください。"
        ),
    }
}

/// 失敗した run の理由を組む（`code` は最初の未処理の失敗 step の code・`fail_reason` は run 単位の理由）。
///
/// run の時間切れ（`fail_reason = run_timeout`）は step の code より優先する（取り消された step の code は
/// 時間切れの結果でしかない）。
#[must_use]
pub fn run_failure(run_id: Uuid, code: Option<&str>, fail_reason: Option<&str>) -> RunFailure {
    let code = if fail_reason == Some("run_timeout") {
        Some("run_timeout")
    } else {
        code
    };
    let category = code.map_or(FailureCategory::Unknown, category_of);
    RunFailure {
        category,
        message: message(category, code),
        reference: run_id.simple().to_string()[..8].to_string(),
    }
}

/// 失敗した run の理由にする code（最初の未処理の失敗 step の code と、map の要素の側の最初の失敗の code から）。
///
/// map の要素の失敗で map が落ちたとき（`map_item_failed`・`item_failed`）と、失敗 step の code が無いときは、
/// 要素の側の code で分類する。画面に返す結果（GAP-09）と実行履歴の詳細が同じ決め方をする。
#[must_use]
pub fn failure_code<'a>(step_code: Option<&'a str>, item_code: Option<&'a str>) -> Option<&'a str> {
    match step_code {
        Some("map_item_failed" | "item_failed") | None => item_code.or(step_code),
        other => other,
    }
}

/// 再開しても同じ結果になる code（BUG-132）。
///
/// 再開は、この実行のバージョンの設定（ピンした IR）・実行の入力・きっかけの内容・完了したブロックの結果
/// （checkpoint）をそのまま使って、失敗したブロックをもう一度実行する。ここに置くのは、それらだけで決まる失敗
/// （外の状態を直しても変わらない）。
///
/// 読めないファイル（`not_text_file`）は置かない。再開は同じファイルの最新のバージョンを読み直すので、作成者が
/// ファイルを UTF-8 か Shift_JIS で保存し直せば通る（原因を直してから再開する・最初から実行し直すと、完了した
/// ブロックの書き込みや送信がもう一度行われる）。
pub const SAME_RESULT_CODES: &[&str] = &[
    "bad_params",
    "expr_resolve_error",
    "expr_type_error",
    "unknown_node",
    "unsupported_stage_a",
    "fanout_limit_exceeded",
    EMPTY_REGION,
    crate::nodes::decide::DECISION_STATE_TOO_LARGE,
    // 使ってよい操作の天井は、ピンした IR の宣言（`declared_scopes`）で決まる。
    "out_of_scope",
];

/// [`SAME_RESULT_CODES`] のうち、この実行のバージョンの設定（ピンした IR）だけで決まる code。
///
/// 「同じ入力でもう一度実行」も同じバージョンで新しく実行するので、作成者が設定を直して新しいバージョンを保存した
/// 後でも、同じ失敗をくり返す。そのため勧めず、エディターで直してから実行し直すよう言う。前のブロックの結果で
/// 変わりうる code（参照の解決・型・params の値・件数の上限など）は置かない（最初から実行し直すと前のブロックの
/// 結果が変わりうる）。
pub const PINNED_VERSION_CODES: &[&str] = &[
    "unknown_node",
    "unsupported_stage_a",
    "out_of_scope",
    EMPTY_REGION,
];

/// 設定の誤りの分類だが、AI の答えが毎回変わるので、再開すると通ることがある code（BUG-132）。
const AI_ANSWER_CODES: &[&str] = &[
    crate::nodes::llm_structured::LLM_OUTPUT_INVALID,
    crate::nodes::llm_structured::LLM_OUTPUT_TRUNCATED,
    crate::nodes::llm_structured::LLM_REFUSED,
    "agent_loop",
];

/// 失敗した run を「失敗したところから再開」したときに、結果が変わりうるか（BUG-132・上の 2 つの表と分類から）。
///
/// - 実行の期限で止めた run（`fail_reason = run_timeout`）は、再開しても実行の期限を延ばさないので、すぐに止まる。
/// - [`SAME_RESULT_CODES`] は再開しても同じ結果になる。
/// - 一時的な問題・時間切れ（ブロック・承認の期限・変更を待つ期限）・AI の答え・分類できない失敗は、そのまま再開できる。
/// - 権限・重なり・見つからない・利用の上限と、ほかの設定の誤り（列・モデル・状態の進め方・相手のサービスなど、
///   この実行の外にあるもの）は、原因を直してから再開する。
#[must_use]
pub fn resume_advice(code: Option<&str>, fail_reason: Option<&str>) -> ResumeAdvice {
    use FailureCategory as F;
    if fail_reason == Some("run_timeout") {
        return ResumeAdvice::PastDeadline;
    }
    let Some(code) = code else {
        return ResumeAdvice::Resume;
    };
    if SAME_RESULT_CODES.contains(&code) {
        return ResumeAdvice::SameResult;
    }
    if AI_ANSWER_CODES.contains(&code) {
        return ResumeAdvice::Resume;
    }
    match category_of(code) {
        F::Temporary | F::Timeout | F::Unknown => ResumeAdvice::Resume,
        F::Permission | F::Conflict | F::NotFound | F::Quota | F::Configuration => {
            ResumeAdvice::FixFirst
        }
    }
}

/// 再開の勧めと、ボタンの近くに出す理由（作成者向け・付録 C の §3.4.4・P6）。
///
/// 「同じ入力でもう一度実行」（この実行のバージョンで新しく実行する）は、この実行のバージョンの設定だけで決まる
/// 失敗（[`PINNED_VERSION_CODES`]）では勧めない（`offer_rerun = false`）。
#[must_use]
pub fn resume_hint(code: Option<&str>, fail_reason: Option<&str>) -> ResumeHint {
    let advice = resume_advice(code, fail_reason);
    let pinned = advice == ResumeAdvice::SameResult
        && code.is_some_and(|c| PINNED_VERSION_CODES.contains(&c));
    let note = match advice {
        ResumeAdvice::Resume => {
            "失敗したブロックから続けます。完了したブロックは実行し直しません。"
        }
        ResumeAdvice::FixFirst => {
            "権限やデータなど、失敗の原因を直してから再開すると、失敗したブロックから続けます。\
             完了したブロックは実行し直しません。"
        }
        ResumeAdvice::SameResult if pinned => {
            "この実行のバージョンの設定で決まる失敗なので、再開しても、同じ入力でもう一度実行しても、\
             同じ結果になります。エディターで設定を直して保存してから、「実行」で実行し直してください。\
             前の実行で済んだ書き込みや送信も、もう一度行われます。"
        }
        ResumeAdvice::SameResult => {
            "失敗したブロックを同じ設定と値でもう一度実行するので、再開しても同じ結果になります。\
             設定か入力を直してから、エディターの「実行」で実行し直してください。\
             「同じ入力でもう一度実行」は、この実行のバージョンの設定で動きます。\
             最初から実行し直すと、前の実行で済んだ書き込みや送信も、もう一度行われます。"
        }
        ResumeAdvice::PastDeadline => {
            "実行の期限を過ぎているため、再開してもすぐに止まります。\
             「同じ入力でもう一度実行」で、最初から実行し直せます。\
             前の実行で済んだ書き込みや送信も、もう一度行われます。"
        }
    };
    ResumeHint {
        advice,
        offer: matches!(advice, ResumeAdvice::Resume | ResumeAdvice::FixFirst),
        offer_rerun: !pinned,
        note: note.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::collections::BTreeSet;
    use std::path::Path;

    #[test]
    fn classifies_codes_and_hides_raw_messages() {
        let id = Uuid::parse_str("0123abcd-0000-0000-0000-000000000000").unwrap();
        let f = run_failure(id, Some("conflict"), None);
        assert_eq!(f.category, FailureCategory::Conflict);
        assert_eq!(f.reference, "0123abcd");
        let f = run_failure(id, Some("unique_violation"), None);
        assert_eq!(f.category, FailureCategory::Conflict);
        assert!(f.message.contains("同じ値がすでに登録"), "{}", f.message);
        // 同じ名前のファイルは、ほかの操作との重なりと言わない（BUG-97）。
        let f = run_failure(id, Some("name_taken"), None);
        assert_eq!(f.category, FailureCategory::Conflict);
        assert_eq!(
            f.message,
            "同じ名前のファイルがすでにあるため、保存できませんでした。"
        );
        assert_eq!(
            run_failure(id, Some("forbidden"), None).category,
            FailureCategory::Permission
        );
        // 承認を待つ間の実行主体の失権は権限の分類（D-62）。文言は何が起きたかを言う（BUG-113）。
        let f = run_failure(id, Some("actor_unavailable"), Some("actor_unavailable"));
        assert_eq!(f.category, FailureCategory::Permission);
        assert!(
            f.message.contains("テーブルを使えなくなった"),
            "{}",
            f.message
        );
        // run の時間切れは step の code より優先する。
        assert_eq!(
            run_failure(id, Some("cancelled"), Some("run_timeout")).category,
            FailureCategory::Timeout
        );
        // 構造化出力の失敗は作成者が設定（プロンプト・出力の形・max_tokens）を見直す分類（GAP-13）。
        for code in ["llm_output_invalid", "llm_output_truncated", "llm_refused"] {
            let f = run_failure(id, Some(code), None);
            assert_eq!(f.category, FailureCategory::Configuration, "{code}");
            assert!(f.message.contains("作成者に連絡"), "{code}: {}", f.message);
        }
        assert_eq!(
            run_failure(id, Some("never-seen"), None).category,
            FailureCategory::Unknown
        );
        assert_eq!(
            run_failure(id, None, None).category,
            FailureCategory::Unknown
        );
    }

    /// run の `fail_reason` に書く理由は、分類の文言より詳しく言う（web の対応表の代わり・BUG-113）。
    #[test]
    fn run_level_reasons_say_what_happened() {
        let id = Uuid::nil();
        let cases = [
            ("run_timeout", "実行の期限までに終わらなかった"),
            ("approval_timeout", "承認の期限までに判断がなかった"),
            (
                "wait_timeout",
                "待つ期限までに変更が起きなかったため、実行を止めました。",
            ),
            ("actor_unavailable", "テーブルを使えなくなった"),
        ];
        for (reason, want) in cases {
            // 承認・変更を待つ理由は step の code と run の fail_reason に同じ値を書く。run の時間切れは
            // fail_reason だけに書く（取り消した step の code より優先する）。
            let step_code = (reason != "run_timeout").then_some(reason);
            let f = run_failure(id, step_code, Some(reason));
            assert!(f.message.contains(want), "{reason}: {}", f.message);
            // 内部の言葉（委譲・コード）を出さない（付録 C の P1）。
            assert!(
                !f.message.contains("委譲") && !f.message.contains(reason),
                "{reason}: {}",
                f.message
            );
        }
        assert_eq!(
            run_failure(id, Some("cancelled"), Some("run_timeout")).message,
            specific_message("run_timeout").unwrap()
        );
    }

    /// 再開しても同じ結果になる失敗では、再開を勧めず理由を言う（BUG-132）。
    #[test]
    fn resume_is_not_offered_when_it_would_fail_the_same_way() {
        // 設定・値で決まる失敗は同じ結果。最初から実行し直すよう言うので、済んだ書き込みや送信が二重になることも言う。
        for code in SAME_RESULT_CODES {
            let h = resume_hint(Some(code), None);
            assert_eq!(h.advice, ResumeAdvice::SameResult, "{code}");
            assert!(!h.offer, "{code}");
            assert!(h.note.contains("同じ結果"), "{code}: {}", h.note);
            assert!(
                h.note
                    .contains("前の実行で済んだ書き込みや送信も、もう一度行われます"),
                "{code}: {}",
                h.note
            );
            // この実行のバージョンの設定だけで決まる失敗は、「同じ入力でもう一度実行」もくり返すので勧めない。
            let pinned = PINNED_VERSION_CODES.contains(code);
            assert_eq!(h.offer_rerun, !pinned, "{code}");
            assert_eq!(
                h.note.contains("エディターで設定を直して保存してから"),
                pinned,
                "{code}: {}",
                h.note
            );
        }
        // 使ってよい操作の天井・知らないブロックは、この実行のバージョンで決まる。参照の解決や件数の上限は、最初から
        // 実行し直すと前のブロックの結果が変わりうる。
        for code in [
            "out_of_scope",
            "unknown_node",
            "unsupported_stage_a",
            EMPTY_REGION,
        ] {
            assert!(!resume_hint(Some(code), None).offer_rerun, "{code}");
        }
        for code in ["expr_resolve_error", "bad_params", "fanout_limit_exceeded"] {
            assert!(resume_hint(Some(code), None).offer_rerun, "{code}");
        }
        // 読めないファイルは、同じファイルを保存し直せば再開で通る（再開は最新のバージョンを読み直す）。
        let h = resume_hint(Some(crate::nodes::ports::NOT_TEXT_FILE), None);
        assert_eq!(h.advice, ResumeAdvice::FixFirst);
        assert!(h.offer && h.offer_rerun, "{h:?}");
        // 実行の期限を過ぎた run は、step の code にかかわらず、再開してもすぐに止まる。
        let h = resume_hint(Some("unavailable"), Some("run_timeout"));
        assert_eq!(h.advice, ResumeAdvice::PastDeadline);
        assert!(!h.offer);
        assert!(h.offer_rerun, "最初から実行し直すのが代わりの手段");
        // 実行の期限で止めた run は、ピンした設定の code でも期限の理由を言う。
        assert_eq!(
            resume_hint(Some("out_of_scope"), Some("run_timeout")).advice,
            ResumeAdvice::PastDeadline
        );
        assert!(h.note.contains("同じ入力でもう一度実行"), "{}", h.note);
        assert!(h.note.contains("もう一度行われます"), "{}", h.note);
        // 一時的な問題・ブロックの時間切れ・AI の答え・分類できない失敗は、そのまま再開を勧める。
        for code in [
            "unavailable",
            "timeout",
            "approval_timeout",
            "wait_timeout",
            "llm_output_invalid",
            "agent_loop",
            "internal",
            "never-seen",
        ] {
            let h = resume_hint(Some(code), None);
            assert_eq!(h.advice, ResumeAdvice::Resume, "{code}");
            assert!(h.offer && h.offer_rerun, "{code}");
        }
        assert_eq!(resume_advice(None, None), ResumeAdvice::Resume);
        // 外の状態（権限・データ・モデル・状態の進め方・相手のサービス）は、直してから再開する。
        for code in [
            "forbidden",
            "actor_unavailable",
            "not_found",
            "name_taken",
            "quota_exceeded",
            "field_unavailable",
            "model_unavailable",
            "approval_not_decidable",
            "http_error_status",
            "script_error",
            "not_text_file",
        ] {
            let h = resume_hint(Some(code), None);
            assert_eq!(h.advice, ResumeAdvice::FixFirst, "{code}");
            assert!(
                h.offer && h.note.contains("原因を直してから"),
                "{code}: {}",
                h.note
            );
        }
    }

    /// PDF・Office の本文を読めなかった失敗（D-111）: 取り込み待ちは一時的な問題で、そのまま再開を勧める（待てば
    /// 読める）。取り込みに失敗した・取り込めない種類は設定の誤りの分類で、原因を直してから再開する。取り込み待ちと
    /// 取り込みの失敗は、分類の文言（「途中まで処理が進んでいる」「ワークフローの設定に問題があり」）でなく原因を言う。
    #[test]
    fn unindexed_documents_say_whether_to_wait() {
        use crate::nodes::ports::{INDEX_FAILED, INDEX_PENDING, NOT_INDEXABLE};
        let id = Uuid::nil();
        let f = run_failure(id, Some(INDEX_PENDING), None);
        assert_eq!(f.category, FailureCategory::Temporary);
        assert!(
            f.message.contains("取り込み終わっていない"),
            "{}",
            f.message
        );
        assert!(!f.message.contains("途中まで"), "{}", f.message);
        let h = resume_hint(Some(INDEX_PENDING), None);
        assert_eq!(h.advice, ResumeAdvice::Resume);
        assert!(h.offer);

        let f = run_failure(id, Some(INDEX_FAILED), None);
        assert_eq!(f.category, FailureCategory::Configuration);
        assert!(f.message.contains("保存し直すと"), "{}", f.message);
        assert!(!f.message.contains("設定に問題"), "{}", f.message);
        for code in [INDEX_FAILED, NOT_INDEXABLE] {
            let h = resume_hint(Some(code), None);
            assert_eq!(h.advice, ResumeAdvice::FixFirst, "{code}");
            assert!(h.offer && h.offer_rerun, "{code}");
        }
        assert_eq!(
            run_failure(id, Some(NOT_INDEXABLE), None).category,
            FailureCategory::Configuration
        );
    }

    /// 同じ結果になる code と AI の答えの code は、どれも分類の表にある（綴りの誤りで黙って外れない）。
    #[test]
    fn resume_tables_only_name_known_codes() {
        let known: BTreeSet<&str> = KNOWN_CODES.iter().map(|(c, _)| *c).collect();
        for code in SAME_RESULT_CODES.iter().chain(AI_ANSWER_CODES) {
            assert!(known.contains(code), "{code}");
        }
        // ピンした設定で決まる code は、どれも再開しても同じ結果になる code。
        for code in PINNED_VERSION_CODES {
            assert!(SAME_RESULT_CODES.contains(code), "{code}");
        }
    }

    /// map の要素の失敗で落ちた run は、要素の側の code で分類する。
    #[test]
    fn failure_code_prefers_the_item_code_for_map_failures() {
        assert_eq!(
            failure_code(Some("forbidden"), Some("x")),
            Some("forbidden")
        );
        assert_eq!(
            failure_code(Some("map_item_failed"), Some("not_text_file")),
            Some("not_text_file")
        );
        assert_eq!(failure_code(Some("item_failed"), None), Some("item_failed"));
        assert_eq!(failure_code(None, Some("unavailable")), Some("unavailable"));
        assert_eq!(failure_code(None, None), None);
    }

    /// 一時障害・時間切れ・分類できない失敗は押し直しを勧めない（別の run になり副作用が二重になり得る）。
    #[test]
    fn retry_prone_categories_ask_to_check_first() {
        let id = Uuid::nil();
        for code in ["unavailable", "timeout", "internal"] {
            let f = run_failure(id, Some(code), None);
            assert!(f.message.contains("確かめてから"), "{code}: {}", f.message);
            assert!(!f.message.contains("押し直"), "{code}");
        }
    }

    /// エンジンと api のポートが出す code は、すべて分類の表にある（分類漏れの検出）。
    ///
    /// `NodeResult::fail("…"`・`PortError::new("…"`・`"code": "…"`・`code: "…"` の形で書いた code を
    /// ソースから拾う（テストのコードは除く）。
    #[test]
    fn every_emitted_code_is_classified() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let re = regex::Regex::new(
            r#"(?:NodeResult::fail|PortError::new)\(\s*"([a-z_]+)"|"code":\s*"([a-z_]+)"|\bcode:\s*"([a-z_]+)""#,
        )
        .unwrap();
        let known: BTreeSet<&str> = KNOWN_CODES.iter().map(|(c, _)| *c).collect();
        let mut missing = BTreeSet::new();
        let mut scanned = 0;
        for dir in [root.join("src"), root.join("../api/src/workflow_runtime")] {
            if !dir.exists() {
                continue;
            }
            let mut stack = vec![dir];
            while let Some(d) = stack.pop() {
                for entry in std::fs::read_dir(&d).unwrap() {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        stack.push(path);
                        continue;
                    }
                    let name = path.file_name().unwrap().to_string_lossy().to_string();
                    let rust = path
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("rs"));
                    if !rust || name.ends_with("tests.rs") {
                        continue;
                    }
                    let src = std::fs::read_to_string(&path).unwrap();
                    let body = src.split("#[cfg(test)]").next().unwrap_or_default();
                    scanned += 1;
                    for c in re.captures_iter(body) {
                        let code = (1..=3).find_map(|i| c.get(i)).unwrap().as_str();
                        if !known.contains(code) {
                            missing.insert(format!("{code}（{}）", path.display()));
                        }
                    }
                }
            }
        }
        assert!(scanned > 10, "ソースを走査できていない");
        assert!(missing.is_empty(), "分類の表に無い code: {missing:?}");
    }
}
