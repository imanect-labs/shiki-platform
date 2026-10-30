//! script が捕まえずに投げ返した誤りの分類（D-121・BUG-136・BUG-137）。
//!
//! ゲスト（QuickJS）は捕まえていない例外を `{"ok":false,"error":{"message":"Error: <文>\n    at …","code":"internal"}}`
//! の形で返す。`ShikiError` の `code` と `retryable` は封筒に載らない（ゲストは例外の文しか写さない。ゲストを
//! 再ビルドせずに直すため、ホスト側で写す・docs/script-runtime-guest.md）。
//!
//! そこで、ホスト（[`crate::engine`] の hostcall）が返した誤りを実行ごとに記録しておき、投げ返された例外の文が
//! そのどれかの文と一致すれば、その呼び出しの誤り（元の `code` と `retryable`）とみなす。一致しなければ、作成者の
//! script が投げた誤り（`throw`・`Shiki.fail`・`TypeError` などの書き誤り）として [`SCRIPT_ERROR`] にする。
//!
//! - 封筒の `code` と `retryable` は使わない。ゲストの中の値は作成者が書き換えられるので、ホストが実際に返した
//!   誤りだけを信じる（作成者が `rate_limited` を名乗って、試す回数を使わない送り直しを起こせないようにする）。
//! - 作成者がホストの誤りを捕まえて、同じ文でもう一度投げたもの（`catch (e) { throw e; }`）は、ホストの誤りのまま。
//!   文を変えて投げ直したもの（`throw new Error('…: ' + e.message)`）は作成者の誤り。

use std::fmt::Write as _;

/// 作成者の script が投げた誤り（設定の誤り・利用者向けの分類は workflow-engine の `run::failure`）。
pub const SCRIPT_ERROR: &str = "script_error";

/// ホストが返した誤り 1 件（実行の中で起きた順に記録する）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostError {
    pub(crate) message: String,
    pub(crate) code: String,
    pub(crate) retryable: bool,
}

/// 文の無いホストの誤りをゲストが言い換える文（ゲストの `call` と前置きの `call` が同じ文を使う）。
const EMPTY_HOST_MESSAGE: &str = "host call failed";

/// 応答をゲストへ書けなかったとき（大きすぎる要求・応答をゲストのメモリに置けない）にゲストが言う文。
pub(crate) const HOST_RETURNED_EMPTY: &str = "host returned empty";

/// 投げ返された例外の文（`message`）を、ホストの誤りか作成者の誤りかに分ける（`(文, code, retryable)`）。
///
/// ホストの誤りなら、ホストが返した文・code・retryable を返す（スタックの行は付かない）。作成者の誤りなら、
/// 例外の文をそのまま（スタックの行つき）[`SCRIPT_ERROR`]・やり直さない、で返す。
pub(crate) fn classify(message: &str, host_errors: &[HostError]) -> (String, String, bool) {
    host_errors
        .iter()
        .rev()
        .find(|h| thrown_from(message, h))
        .map_or_else(
            || (message.to_string(), SCRIPT_ERROR.to_string(), false),
            |h| (h.message.clone(), h.code.clone(), h.retryable),
        )
}

/// 例外の文が、そのホストの誤りから作った `Error` のものか（ゲストは `Error: <文>` に続けてスタックの行を書く）。
fn thrown_from(message: &str, h: &HostError) -> bool {
    let shown = if h.message.is_empty() {
        EMPTY_HOST_MESSAGE
    } else {
        h.message.as_str()
    };
    message
        .strip_prefix("Error: ")
        .and_then(|rest| rest.strip_prefix(shown))
        .is_some_and(|tail| tail.is_empty() || tail.starts_with('\n'))
}

/// ゲストの封筒を読む。読めなければ、文字列の中の生の制御文字を逃がしてもう一度読む。
///
/// ゲストは例外の文の `\` `"` 改行だけを逃がして封筒を組むので、タブなどを含む文（ホストの誤りの文にも入りうる）だと
/// JSON として読めない。封筒はゲストが空白を挟まずに組むので、生の制御文字は文字列の中にしか無い。
pub(crate) fn parse_envelope(envelope: &str) -> Option<serde_json::Value> {
    if let Ok(v) = serde_json::from_str(envelope) {
        return Some(v);
    }
    let raw_control = |c: char| u32::from(c) < 0x20;
    if !envelope.chars().any(raw_control) {
        return None;
    }
    let mut escaped = String::with_capacity(envelope.len() + 16);
    for c in envelope.chars() {
        if raw_control(c) {
            let _ = write!(escaped, "\\u{:04x}", u32::from(c));
        } else {
            escaped.push(c);
        }
    }
    serde_json::from_str(&escaped).ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn host(message: &str, code: &str, retryable: bool) -> HostError {
        HostError {
            message: message.into(),
            code: code.into(),
            retryable,
        }
    }

    #[test]
    fn uncaught_host_errors_keep_their_code() {
        let errors = [
            host("見つかりません", "not_found", false),
            host(
                "scope_ceiling 外の Shiki 呼び出し: storage.read",
                "out_of_scope",
                false,
            ),
        ];
        let thrown = "Error: scope_ceiling 外の Shiki 呼び出し: storage.read\n    at mkErr (eval_script:13:17)\n";
        assert_eq!(
            classify(thrown, &errors),
            (
                "scope_ceiling 外の Shiki 呼び出し: storage.read".to_string(),
                "out_of_scope".to_string(),
                false
            )
        );
        // スタックの行が無い形・やり直せる誤りもそのまま写す。
        let errors = [host("混んでいます", "unavailable", true)];
        assert_eq!(
            classify("Error: 混んでいます", &errors).1,
            "unavailable".to_string()
        );
        assert!(classify("Error: 混んでいます", &errors).2);
        // 文の無い誤りは、ゲストが言い換えた文で一致させる。
        let errors = [host("", "internal", false)];
        assert_eq!(
            classify("Error: host call failed\n    at call", &errors).1,
            "internal"
        );
    }

    #[test]
    fn author_errors_are_script_errors() {
        let errors = [host("見つかりません", "not_found", false)];
        for thrown in [
            "Error: mine\n    at main (eval_script:31:15)\n",
            // ホストの文を含んでも、文を変えて投げ直したものは作成者の誤り。
            "Error: 読めません: 見つかりません\n    at main",
            // 続きのある別の文。
            "Error: 見つかりませんでした\n    at main",
            "str",
            "[object Object]",
        ] {
            let (message, code, retryable) = classify(thrown, &errors);
            assert_eq!(code, SCRIPT_ERROR, "{thrown}");
            assert_eq!(message, thrown);
            assert!(!retryable);
        }
        // ホストの誤りが無ければ、どれも作成者の誤り。
        assert_eq!(classify("Error: 見つかりません", &[]).1, SCRIPT_ERROR);
    }

    /// 同じ文の誤りが何度もあれば、いちばん後の呼び出しのもの。
    #[test]
    fn the_latest_matching_host_error_wins() {
        let errors = [
            host("x", "unavailable", true),
            host("x", "not_found", false),
        ];
        assert_eq!(classify("Error: x", &errors).1, "not_found");
    }

    #[test]
    fn envelopes_with_raw_control_characters_are_read() {
        let raw = "{\"ok\":false,\"error\":{\"message\":\"Error: a\tb\\n    at x\",\"code\":\"internal\",\"retryable\":false}}";
        assert!(serde_json::from_str::<serde_json::Value>(raw).is_err());
        let v = parse_envelope(raw).unwrap();
        assert_eq!(v["error"]["message"], "Error: a\tb\n    at x");
        assert!(parse_envelope("not json").is_none());
        assert_eq!(
            parse_envelope("{\"ok\":true,\"value\":1}").unwrap()["value"],
            1
        );
    }
}
