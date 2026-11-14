//! ユーザーの発話から URL を抜き出す（`browser` の出所の初期値・docs/agent/browser.md §6.2）。
//!
//! 出所は「ユーザーが書いた URL ならそのまま開いてよい」という判定に使う。広く拾いすぎると
//! 書いていない URL まで事前許可になるので、`http://` / `https://` で始まる連続した ASCII だけを拾い、
//! 文末の句読点・閉じ括弧は落とす（「〜を見て（https://a.example/x）。」の形）。

use url::Url;

/// 1 会話から拾う URL の上限（出所の集合を有界にする）。
const MAX_URLS: usize = 64;

/// URL の途中に現れない文字（ここで切る）。
fn ends_url(c: char) -> bool {
    !c.is_ascii() || c.is_ascii_whitespace() || c.is_ascii_control() || "<>\"'`{}|\\^".contains(c)
}

/// 文末に付きがちな記号を落とす（対応する開き括弧が URL の中に無い閉じ括弧も落とす）。
fn trim_tail(mut s: &str) -> &str {
    loop {
        let Some(last) = s.chars().last() else {
            return s;
        };
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' => true,
            ')' => s.matches('(').count() < s.matches(')').count(),
            ']' => s.matches('[').count() < s.matches(']').count(),
            _ => false,
        };
        if !drop {
            return s;
        }
        s = &s[..s.len() - last.len_utf8()];
    }
}

/// 本文に書かれた http(s) の URL（重複なし・出現順・上限 [`MAX_URLS`]）。
#[must_use]
pub fn user_urls<'a>(texts: impl IntoIterator<Item = &'a str>) -> Vec<Url> {
    let mut out: Vec<Url> = Vec::new();
    for text in texts {
        // ASCII の小文字化はバイト位置を変えない。
        let lower = text.to_ascii_lowercase();
        let mut from = 0;
        while let Some(pos) = next_scheme(&lower, from) {
            let rest = &text[pos..];
            let end = rest.find(ends_url).unwrap_or(rest.len());
            from = pos + end.max(1);
            let candidate = trim_tail(&rest[..end]);
            let Ok(url) = Url::parse(candidate) else {
                continue;
            };
            if url.host_str().is_none_or(str::is_empty) || out.contains(&url) {
                continue;
            }
            out.push(url);
            if out.len() >= MAX_URLS {
                return out;
            }
        }
    }
    out
}

/// `from` 以降で最初の `http://` / `https://` の位置。
fn next_scheme(lower: &str, from: usize) -> Option<usize> {
    let hay = lower.get(from..)?;
    let http = hay.find("http://");
    let https = hay.find("https://");
    let first = match (http, https) {
        (Some(a), Some(b)) => a.min(b),
        (a, b) => a.or(b)?,
    };
    Some(from + first)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(urls: &[Url]) -> Vec<&str> {
        urls.iter().map(Url::as_str).collect()
    }

    #[test]
    fn picks_urls_and_drops_trailing_punctuation() {
        let urls = user_urls([
            "これを見て（https://a.example/x?q=1）。あと https://b.example/y. と HTTP://C.example/",
            "重複 https://a.example/x?q=1 と <https://d.example/(1)> と https://e.example/です",
        ]);
        assert_eq!(
            strs(&urls),
            [
                "https://a.example/x?q=1",
                "https://b.example/y",
                "http://c.example/",
                "https://d.example/(1)",
                "https://e.example/",
            ]
        );
    }

    #[test]
    fn ignores_non_urls_and_is_bounded() {
        assert!(user_urls(["ftp://x.example/ https:// http://[ mailto:a@b"]).is_empty());
        let many: String = (0..100)
            .map(|i| format!("https://h{i}.example/ "))
            .collect();
        assert_eq!(user_urls([many.as_str()]).len(), MAX_URLS);
    }
}
