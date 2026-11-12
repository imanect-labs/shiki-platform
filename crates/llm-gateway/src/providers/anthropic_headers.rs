//! Anthropic の応答のレート制限のヘッダを、ログにだけ記録する（T00）。
//!
//! 記録するのは `anthropic-ratelimit-{requests,tokens,input-tokens,output-tokens}-{remaining,reset}` だけ
//! （[応答ヘッダの表]）。上限（`-limit`）は組織の階級で決まる値なので出さない。名前を決め打ちで拾うので、本文と
//! `anthropic-organization-id`・`request-id` などの識別子は出ない（BUG-87）。DB・[`StreamDelta`] には載せない
//! （評価での記録は abeval.proxy の側で行う）。
//!
//! [応答ヘッダの表]: https://platform.claude.com/docs/en/api/rate-limits#response-headers
//! [`StreamDelta`]: crate::model::StreamDelta

use std::fmt;

use reqwest::header::HeaderMap;
use reqwest::StatusCode;

/// 記録するヘッダ（ログの項目名・ヘッダ名）。残りは整数、戻る時刻は RFC 3339。
const RECORDED: [(&str, &str); 8] = [
    (
        "requests_remaining",
        "anthropic-ratelimit-requests-remaining",
    ),
    ("requests_reset", "anthropic-ratelimit-requests-reset"),
    ("tokens_remaining", "anthropic-ratelimit-tokens-remaining"),
    ("tokens_reset", "anthropic-ratelimit-tokens-reset"),
    (
        "input_tokens_remaining",
        "anthropic-ratelimit-input-tokens-remaining",
    ),
    (
        "input_tokens_reset",
        "anthropic-ratelimit-input-tokens-reset",
    ),
    (
        "output_tokens_remaining",
        "anthropic-ratelimit-output-tokens-remaining",
    ),
    (
        "output_tokens_reset",
        "anthropic-ratelimit-output-tokens-reset",
    ),
];

/// 記録する値の長さの上限（RFC 3339 の時刻より長いものは記録しない）。
const MAX_VALUE_LEN: usize = 40;

/// レート制限のヘッダを `項目名=値` の空白区切りで書き出す（無いヘッダ・形の外れた値は書かない。1 つも無ければ
/// `none`）。`tracing` の項目に `%` で渡すと、その水準のログが有効なときにだけ書き出す。
pub(super) struct RateLimitHeaders<'a>(pub(super) &'a HeaderMap);

impl fmt::Display for RateLimitHeaders<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut written = false;
        for (label, name) in RECORDED {
            let Some(value) = self
                .0
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|v| plausible(v))
            else {
                continue;
            };
            if written {
                f.write_str(" ")?;
            }
            write!(f, "{label}={value}")?;
            written = true;
        }
        if !written {
            f.write_str("none")?;
        }
        Ok(())
    }
}

/// 残り（整数）・戻る時刻（RFC 3339）に現れる文字だけの短い値か（ログへ任意の文字列を流さない）。
fn plausible(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= MAX_VALUE_LEN
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'.' | b'+' | b'-'))
}

/// 応答ヘッダを受けたところで記録する。成功は debug、429 は warn（429 の本文は
/// [`crate::provider::rate_limit_message`] が別に出す）。ほかの状態は記録しない。
pub(super) fn record(status: StatusCode, headers: &HeaderMap) {
    if status.is_success() {
        tracing::debug!(rate_limit = %RateLimitHeaders(headers), "anthropic のレート制限の残り");
    } else if status == StatusCode::TOO_MANY_REQUESTS {
        tracing::warn!(
            rate_limit = %RateLimitHeaders(headers),
            "anthropic が 429 を返した（送り直さない・レート制限の残り）"
        );
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    use std::sync::{Arc, Mutex};

    const ORG_ID: &str = "5a7f0c4e-org-secret";

    /// 実際の応答に近いヘッダ（記録するもの・上限・識別子・形の外れた値）。
    fn headers() -> HeaderMap {
        let mut h = HeaderMap::new();
        for (name, value) in [
            ("anthropic-ratelimit-requests-limit", "4000"),
            ("anthropic-ratelimit-requests-remaining", "3999"),
            ("anthropic-ratelimit-requests-reset", "2026-11-12T03:04:05Z"),
            ("anthropic-ratelimit-tokens-remaining", "2399000"),
            ("anthropic-ratelimit-input-tokens-remaining", " 1999000 "),
            (
                "anthropic-ratelimit-input-tokens-reset",
                "2026-11-12T03:04:05.123+00:00",
            ),
            // 形の外れた値（空白・記号を含む）は書かない。
            (
                "anthropic-ratelimit-output-tokens-remaining",
                "400 000; x=1",
            ),
            ("anthropic-organization-id", ORG_ID),
            ("request-id", "req_011CSecret"),
            ("retry-after", "3"),
        ] {
            h.insert(name, value.parse().unwrap());
        }
        h
    }

    #[test]
    fn only_remaining_and_reset_are_written() {
        assert_eq!(
            RateLimitHeaders(&headers()).to_string(),
            "requests_remaining=3999 requests_reset=2026-11-12T03:04:05Z \
             tokens_remaining=2399000 input_tokens_remaining=1999000 \
             input_tokens_reset=2026-11-12T03:04:05.123+00:00"
        );
        assert_eq!(RateLimitHeaders(&HeaderMap::new()).to_string(), "none");
    }

    /// ログの書き出し先（試験で受ける・anthropic の試験も使う）。
    #[derive(Clone, Default)]
    pub(in crate::providers) struct Logs(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Logs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
        type Writer = Logs;

        fn make_writer(&'a self) -> Logs {
            self.clone()
        }
    }

    impl Logs {
        /// debug 以上を受ける購読者（時刻は付けない）。戻り値を持っている間だけ、このスレッドのログを受ける。
        pub(in crate::providers) fn capture(&self) -> tracing::subscriber::DefaultGuard {
            let subscriber = tracing_subscriber::fmt()
                .with_writer(self.clone())
                .with_ansi(false)
                .without_time()
                .with_max_level(tracing::Level::DEBUG)
                .finish();
            tracing::subscriber::set_default(subscriber)
        }

        pub(in crate::providers) fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    /// 成功は debug・429 は warn で残りと戻る時刻を出し、上限・組織の ID・要求の ID は出さない。ほかの状態は出さない。
    #[test]
    fn record_logs_remaining_without_identifiers() {
        let logs = Logs::default();
        let _guard = logs.capture();
        let h = headers();
        record(StatusCode::OK, &h);
        record(StatusCode::TOO_MANY_REQUESTS, &h);
        record(StatusCode::BAD_REQUEST, &h);
        let text = logs.text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(
            lines[0].contains("DEBUG") && lines[1].contains("WARN"),
            "{text}"
        );
        for line in lines {
            assert!(line.contains("requests_remaining=3999"), "{text}");
            assert!(line.contains("input_tokens_reset=2026-11-12T03:04:05.123+00:00"));
        }
        for hidden in [ORG_ID, "req_011CSecret", "4000", "400 000"] {
            assert!(!text.contains(hidden), "{hidden}: {text}");
        }
    }
}
