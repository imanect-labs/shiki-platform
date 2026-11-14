//! ユーザーの発話から URL を抜き出す（`browser` の出所の初期値・docs/agent/browser.md §6.2）。
//!
//! 出所は「ユーザーが書いた URL ならそのまま開いてよい」という判定に使う。広く拾いすぎると
//! 書いていない URL まで事前許可になるので、`http://` / `https://` から空白・全角の句読点や括弧までを 1 つの候補にし、
//! 文末の句読点・閉じ括弧は落とす（「〜を見て（https://a.example/x）。」の形）。途中で切って別の URL に
//! しない（読めない候補は捨てる）。

use url::Url;

/// 1 会話から拾う URL の上限（出所の集合を有界にする）。
const MAX_URLS: usize = 64;

/// URL を区切る文字（ここで切る）。
///
/// 非 ASCII 一般では切らない。`https://shop.example.日本/` を ASCII の途中で切ると、ユーザーが書いていない
/// 別オリジン（`https://shop.example/`）を出所に入れてしまう。候補は丸ごと `Url::parse` に渡し、国際化ドメインと
/// パスの非 ASCII はそちらで正規化する（`https://a.example/です` のように文が続いても、出所に入るのはその
/// 正規化された URL だけで、別のオリジンは増えない）。切るのは空白・括弧・句読点などの区切りだけ。
fn ends_url(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || "<>\"'`{}|\\^".contains(c)
        || "、。，．「」『』（）【】〈〉《》［］｛｝！？：；…・＂＇".contains(c)
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
                "https://e.example/%E3%81%A7%E3%81%99",
            ]
        );
    }

    /// 非 ASCII の途中で切って、書いていない別オリジンを出所に入れない（Codex 指摘）。
    #[test]
    fn unicode_hosts_are_not_cut_into_another_origin() {
        let urls = user_urls(["https://shop.example.日本/ と https://例え.テスト/パス。"]);
        assert_eq!(
            strs(&urls),
            [
                "https://shop.example.xn--wgv71a/",
                "https://xn--r8jz45g.xn--zckzah/%E3%83%91%E3%82%B9",
            ]
        );
        assert!(urls.iter().all(|u| u.host_str() != Some("shop.example")));
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
