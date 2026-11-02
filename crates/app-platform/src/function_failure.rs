//! B2 関数の失敗を、呼び出し側（アプリ・自動実行の記録）へ返す形にする（GAP-60）。
//!
//! script の実行の誤り（`(文, code, やり直せるか)`・[`script_runtime::engine::ExecOutcome`]）から、
//! 利用者と作成者に見せてよい文を作る。返さないもの:
//! - スタックの行（`    at mkErr (eval_script:13:17)`）と、例外の型の前置き `Error: `
//! - 内部の文（ゲスト・ランタイムの英語の原文、トラップの内容、フレーム違反の中身）
//! - 下位の通信の誤りの原文（ゲートウェイのループバックの URL・外部の URL）
//!
//! 文を返すのは、作成者が書いた誤り（`script_error`）と、ホストが日本語で決めた文（ゲートウェイの 4xx の断り・
//! 引数の誤り・使えない能力・外部への送信を断った理由）だけで、どちらも最初の行を最大 [`MAX_MESSAGE_CHARS`] 文字にする。ほかは code ごとに
//! 決まった文にする。

use script_runtime::engine::Termination;

/// 返す文の長さの上限（文字数）。
const MAX_MESSAGE_CHARS: usize = 300;

/// 関数の失敗（呼び出し側へ返す・GAP-60）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FunctionFailure {
    /// 理由。作成者が投げた誤りは `script_error`、上限で止めたものは `resource`、ゲートウェイが断ったものは
    /// `http_<状態>`（例 `http_403`）、外部への通信は `egress_denied`・`egress_failed`、分からないものは `internal`。
    pub code: String,
    /// 利用者と作成者に見せてよい文（スタックの行・内部の文・URL を含まない）。
    pub message: String,
    /// もう一度呼べば通るかもしれないか。
    pub retryable: bool,
}

/// 実行の誤りと中断の理由から、返す失敗を作る。
pub(crate) fn function_failure(
    error: Option<(String, String, bool)>,
    termination: &Termination,
    time_limit_secs: u64,
) -> FunctionFailure {
    let (raw, code, retryable) =
        error.unwrap_or_else(|| (String::new(), "internal".to_string(), false));
    let message = match code.as_str() {
        script_runtime::SCRIPT_ERROR => {
            let line = first_line(raw.strip_prefix("Error: ").unwrap_or(&raw));
            if line.is_empty() {
                "関数が誤りを返しました。".to_string()
            } else {
                line
            }
        }
        "resource" => match termination {
            Termination::Epoch => {
                format!("時間の上限（{time_limit_secs} 秒）に達したため、関数を止めました。")
            }
            Termination::Memory => "メモリーの上限に達したため、関数を止めました。".to_string(),
            _ => "計算の量が上限に達したため、関数を止めました。".to_string(),
        },
        "frame_violation" => "関数が許されていない呼び出しをしたため、止めました。".to_string(),
        "cancelled" => "関数の実行が取り消されました。".to_string(),
        "gateway_unreachable" => {
            "ゲートウェイに届きませんでした。しばらくしてからもう一度試してください。".to_string()
        }
        "egress_failed" => "外部のサービスに接続できませんでした。".to_string(),
        // ホストが日本語で決めた文（引数の誤り・B2 関数で使えない能力・外部への送信を断った理由。断った理由は、許可して
        // いない先・決まったポート以外・内部のネットワーク・その名前のホストが無いで、解決したアドレスは含まない・BUG-196）。
        "invalid" | "unsupported" | "unknown_api" | "egress_denied" if !raw.is_empty() => {
            first_line(&raw)
        }
        "egress_denied" => {
            "このアプリが通信してよい先に含まれていないため、外部のサービスに接続しませんでした。"
                .to_string()
        }
        c if is_gateway_rejection(c) && !raw.is_empty() => first_line(&raw),
        c if c.starts_with("http_") => {
            "ゲートウェイで問題が起きました。しばらくしてからもう一度試してください。".to_string()
        }
        _ => "関数の実行中に問題が起きました。".to_string(),
    };
    FunctionFailure {
        code,
        message,
        retryable,
    }
}

/// ゲートウェイが 4xx で断った誤りか（文はゲートウェイが利用者向けに決めたもの）。
fn is_gateway_rejection(code: &str) -> bool {
    code.strip_prefix("http_4")
        .is_some_and(|rest| rest.len() == 2 && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// 最初の行（前後の空白を外し、上限の文字数で切る）。
fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    let mut out: String = line.chars().take(MAX_MESSAGE_CHARS).collect();
    if line.chars().count() > MAX_MESSAGE_CHARS {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::needless_pass_by_value)] // 呼び出し側で中断の理由をその場で作るため値で受ける。
    fn fail(message: &str, code: &str, termination: Termination) -> FunctionFailure {
        function_failure(
            Some((message.to_string(), code.to_string(), false)),
            &termination,
            20,
        )
    }

    /// 作成者が投げた誤りは、文の最初の行だけを返す（スタックの行と `Error: ` は返さない）。
    #[test]
    fn author_errors_keep_only_their_first_line() {
        let f = fail(
            "Error: 金額が空です\n    at main (eval_script:3:11)\n",
            "script_error",
            Termination::Completed,
        );
        assert_eq!(f.code, "script_error");
        assert_eq!(f.message, "金額が空です");
        let typed = fail(
            "TypeError: cannot read property 'amount' of undefined\n    at main (eval_script:2:5)",
            "script_error",
            Termination::Completed,
        );
        assert_eq!(
            typed.message,
            "TypeError: cannot read property 'amount' of undefined"
        );
        assert!(!typed.message.contains("eval_script"));
        let long = fail(&"あ".repeat(400), "script_error", Termination::Completed);
        assert_eq!(long.message.chars().count(), MAX_MESSAGE_CHARS + 1);
        assert_eq!(
            fail("", "script_error", Termination::Completed).message,
            "関数が誤りを返しました。"
        );
    }

    /// 内部の文・下位の通信の誤りの原文は返さず、code ごとに決まった文にする。
    #[test]
    fn internal_texts_are_not_returned() {
        let internal = fail(
            "wasm trap: out of bounds memory access",
            "internal",
            Termination::Trap("wasm trap".into()),
        );
        assert_eq!(internal.message, "関数の実行中に問題が起きました。");
        let unreachable = fail(
            "ゲートウェイ呼び出しに失敗: error sending request for url (http://127.0.0.1:8090/gw/data/tables)",
            "gateway_unreachable",
            Termination::Completed,
        );
        assert!(
            !unreachable.message.contains("127.0.0.1"),
            "{unreachable:?}"
        );
        let egress = fail(
            "外部 HTTP 失敗: error sending request for url (https://api.partner.example/x?key=secret)",
            "egress_failed",
            Termination::Completed,
        );
        assert!(!egress.message.contains("secret"), "{egress:?}");
        let frame = fail(
            "frame violation: unknown api fs.read",
            "frame_violation",
            Termination::FrameViolation("unknown api fs.read".into()),
        );
        assert!(!frame.message.contains("fs.read"), "{frame:?}");
        let server = fail(
            "内部エラー: db: connection refused",
            "http_500",
            Termination::Completed,
        );
        assert!(!server.message.contains("db"), "{server:?}");
        let missing = function_failure(None, &Termination::Completed, 20);
        assert_eq!(missing.code, "internal");
    }

    /// ゲートウェイの 4xx の断りとホストの引数の誤りは、決まった日本語の文なのでそのまま返す。上限は種類を言う。
    #[test]
    fn gateway_rejections_and_limits_are_explained() {
        let denied = fail(
            "権限がありません: この操作は許可されていません",
            "http_403",
            Termination::Completed,
        );
        assert_eq!(denied.code, "http_403");
        assert_eq!(
            denied.message,
            "権限がありません: この操作は許可されていません"
        );
        assert_eq!(
            fail("table_id が不正です", "invalid", Termination::Completed).message,
            "table_id が不正です"
        );
        assert_eq!(
            fail("time limit exceeded", "resource", Termination::Epoch).message,
            "時間の上限（20 秒）に達したため、関数を止めました。"
        );
        assert!(fail("fuel exhausted", "resource", Termination::Fuel)
            .message
            .contains("計算の量"));
        assert!(
            fail("memory limit exceeded", "resource", Termination::Memory)
                .message
                .contains("メモリー")
        );
        // 外部への送信を断った理由（内部のネットワークにつながる・許可していない先）は、ホストの文のまま返す（BUG-196）。
        assert_eq!(
            fail(
                "api.example.com は内部のネットワークにつながるため、接続しませんでした。",
                "egress_denied",
                Termination::Completed
            )
            .message,
            "api.example.com は内部のネットワークにつながるため、接続しませんでした。"
        );
        assert!(fail("", "egress_denied", Termination::Completed)
            .message
            .contains("通信してよい先"));
        // http_4 の後が 2 桁の数でないものは、ゲートウェイの断りとして扱わない。
        assert!(!is_gateway_rejection("http_4"));
        assert!(!is_gateway_rejection("http_40x"));
        assert!(is_gateway_rejection("http_404"));
    }
}
