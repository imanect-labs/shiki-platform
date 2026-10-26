//! 失敗した run の理由を、利用者向けの分類と文言に置き換える（GAP-09・1 か所で定義する）。
//!
//! step の `error.code` と run の `fail_reason`（run_timeout）から分類を決める。生の `message`（英語の原文・
//! DB の詳細・URL・値を含む競合の詳細など）は返さない。分類は読取のときに決め、終端で書き込まない
//! （再開した run に古い理由を残さない）。
//!
//! 一時障害・時間切れ・分類できない失敗の文言は、押し直しを勧めない。押し直すと新しい冪等キーで別の run に
//! なり、途中まで進んだ副作用が二重になり得る（PIT-31）。

use uuid::Uuid;

use super::outcome::{FailureCategory, RunFailure};

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
        // 競合
        ("conflict", Conflict),
        ("effect_conflict", Conflict),
        ("unique_violation", Conflict),
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
        ("effect_in_progress", Temporary),
        ("concurrency_limited", Temporary),
        ("fetch_failed", Temporary),
        // 利用の上限
        ("quota_exceeded", Quota),
        ("sandbox_limit", Quota),
        // AI エージェントが手順・時間・使える量の上限に達しても回答を書けなかった（BUG-6）。
        ("agent_incomplete", Quota),
        // 時間切れ
        ("timeout", Timeout),
        ("run_timeout", Timeout),
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
        ("fanout_limit_exceeded", Configuration),
        ("model_unavailable", Configuration),
        ("model_choice_disabled", Configuration),
        ("no_model", Configuration),
        ("approval_not_decidable", Configuration),
        ("approval_no_assignee", Configuration),
        ("approval_assignee_unreadable", Configuration),
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
    ]
};

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

/// 分類の文言（`unique_violation` だけは同じ値があることを伝える）。
fn message(category: FailureCategory, code: Option<&str>) -> String {
    use FailureCategory as F;
    if code == Some("unique_violation") {
        return "同じ値がすでに登録されているため、登録できませんでした。".to_string();
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
        assert_eq!(
            run_failure(id, Some("forbidden"), None).category,
            FailureCategory::Permission
        );
        // run の時間切れは step の code より優先する。
        assert_eq!(
            run_failure(id, Some("cancelled"), Some("run_timeout")).category,
            FailureCategory::Timeout
        );
        assert_eq!(
            run_failure(id, Some("never-seen"), None).category,
            FailureCategory::Unknown
        );
        assert_eq!(
            run_failure(id, None, None).category,
            FailureCategory::Unknown
        );
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
