//! 回答の画像の代理取得（#1044）。宛先の安全確認は `web_fetch` と同じ関門（`public_fetch`）を通る。
//!
//! - リダイレクトは [`MAX_REDIRECTS`] 回まで追い、**1 回ごとに URL 検証と解決後 IP の検証をやり直す**
//!   （画像 CDN は 3xx を返すことが多いが、検証を迂回する誘導にもなる・PIT-36）。
//! - 時間（[`FETCH_TIMEOUT`]）と大きさ（[`MAX_IMAGE_BYTES`]）に上限を掛ける。
//! - **中身のマジックバイト**で png / jpeg / gif / webp / avif だけを通す。宣言された Content-Type は
//!   信じない（`image/png` と名乗る HTML も、スクリプトを持てる SVG も通さない）。返す Content-Type も
//!   判定した型にする。

use std::time::Duration;

use agent_core::tools::public_fetch::{self, AddrGuard, PinError};
use sandbox_client::net_guard::HostResolver;

/// リダイレクトを追う上限。
pub(crate) const MAX_REDIRECTS: usize = 3;
/// 1 回の取得の期限（接続〜読み切り）。
pub(crate) const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// 画像の大きさの上限。
pub(crate) const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

const USER_AGENT: &str = "shiki-image-proxy/1.0";
const ACCEPT: &str = "image/avif,image/webp,image/png,image/jpeg,image/gif;q=0.9";

/// 取得した画像（中身から判定した型）。
#[derive(Debug)]
pub(crate) struct FetchedImage {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
}

/// 取得の失敗。理由はログにだけ残し、クライアントへは返さない（取得先の内部事情を漏らさない）。
#[derive(Debug)]
pub(crate) enum ImageFetchError {
    /// 宛先が検証を通らない（URL 不正・非公開アドレス・名前解決失敗）。
    Rejected(String),
    /// 取得先が失敗を返した・接続できなかった。
    Upstream(String),
    /// 許可した画像形式ではない。
    NotImage,
    /// 上限を超えた。
    TooLarge,
    /// リダイレクトが多すぎる。
    TooManyRedirects,
}

impl ImageFetchError {
    /// ログに残す理由。
    pub(crate) fn reason(&self) -> &str {
        match self {
            ImageFetchError::Rejected(r) | ImageFetchError::Upstream(r) => r,
            ImageFetchError::NotImage => "許可した画像形式ではない",
            ImageFetchError::TooLarge => "上限を超えた",
            ImageFetchError::TooManyRedirects => "リダイレクトが多すぎる",
        }
    }
}

/// `url` の画像を取得する。`guard` は解決後 IP の検証（本番は [`public_fetch::public_only`]）。
pub(crate) async fn fetch_image(
    resolver: &dyn HostResolver,
    url: &str,
    guard: AddrGuard,
) -> Result<FetchedImage, ImageFetchError> {
    let mut current = url.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        let target = public_fetch::validate_url(&current).map_err(ImageFetchError::Rejected)?;
        let client =
            public_fetch::pinned_client(resolver, &target, guard, FETCH_TIMEOUT, USER_AGENT)
                .await
                .map_err(|e| match e {
                    PinError::Client(e) => ImageFetchError::Upstream(e),
                    other => ImageFetchError::Rejected(format!("{other:?}")),
                })?;
        let response = client
            .get(target.url.clone())
            .header(reqwest::header::ACCEPT, ACCEPT)
            .send()
            .await
            .map_err(|e| ImageFetchError::Upstream(e.to_string()))?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| ImageFetchError::Upstream(format!("{status} without Location")))?;
            // 相対の Location は今の URL を基準に解く。次の周回で検証し直す。
            current = target
                .url
                .join(location)
                .map_err(|e| ImageFetchError::Rejected(e.to_string()))?
                .into();
            continue;
        }
        if !status.is_success() {
            return Err(ImageFetchError::Upstream(status.to_string()));
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_IMAGE_BYTES as u64)
        {
            return Err(ImageFetchError::TooLarge);
        }
        let (bytes, capped) = public_fetch::read_capped(response, MAX_IMAGE_BYTES)
            .await
            .map_err(|e| ImageFetchError::Upstream(e.to_string()))?;
        if capped {
            return Err(ImageFetchError::TooLarge);
        }
        let mime = sniff_image(&bytes).ok_or(ImageFetchError::NotImage)?;
        return Ok(FetchedImage { bytes, mime });
    }
    Err(ImageFetchError::TooManyRedirects)
}

/// 中身の先頭から画像形式を判定する。許可するのはラスタ画像だけ（SVG は通さない）。
pub(crate) fn sniff_image(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if b.len() >= 12 && &b[4..8] == b"ftyp" && matches!(&b[8..12], b"avif" | b"avis") {
        return Some("image/avif");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];

    #[test]
    fn sniffs_only_allowed_raster_formats() {
        assert_eq!(sniff_image(PNG), Some("image/png"));
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff_image(b"GIF89a...."), Some("image/gif"));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_image(b"\0\0\0\x1cftypavif\0\0"), Some("image/avif"));
        assert_eq!(
            sniff_image(b"<svg xmlns='http://www.w3.org/2000/svg'>"),
            None
        );
        assert_eq!(sniff_image(b"<!doctype html><script>"), None);
        assert_eq!(sniff_image(b""), None);
    }

    /// すべての名前をループバックのスタブへ向けるリゾルバ（テスト専用）。
    struct ToStub(SocketAddr);
    #[async_trait::async_trait]
    impl HostResolver for ToStub {
        async fn lookup(&self, _: &str, _: u16) -> Result<Vec<SocketAddr>, &'static str> {
            Ok(vec![self.0])
        }
    }

    fn allow_loopback(a: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, &'static str> {
        Ok(a)
    }

    /// 応答を順に返す最小の HTTP スタブ。受けた要求数も数える。
    async fn stub(responses: Vec<Vec<u8>>) -> (SocketAddr, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        tokio::spawn(async move {
            for resp in responses {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0u8; 4096];
                let _ = sock.read(&mut buf).await;
                h.fetch_add(1, Ordering::SeqCst);
                let _ = sock.write_all(&resp).await;
                let _ = sock.shutdown().await;
            }
        });
        (addr, hits)
    }

    fn http(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
        let mut v = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
            body.len()
        )
        .into_bytes();
        v.extend_from_slice(body);
        v
    }

    #[tokio::test]
    async fn fetches_png_and_uses_sniffed_type() {
        let (addr, _) = stub(vec![http("200 OK", "Content-Type: text/html\r\n", PNG)]).await;
        let url = format!("http://img.example.com:{}/a.png", addr.port());
        let img = fetch_image(&ToStub(addr), &url, allow_loopback)
            .await
            .unwrap();
        assert_eq!(img.mime, "image/png", "宣言ではなく中身で決める");
    }

    #[tokio::test]
    async fn rejects_svg_even_if_declared_as_image() {
        let body = b"<svg xmlns='http://www.w3.org/2000/svg'><script>alert(1)</script></svg>";
        let (addr, _) = stub(vec![http(
            "200 OK",
            "Content-Type: image/svg+xml\r\n",
            body,
        )])
        .await;
        let url = format!("http://img.example.com:{}/a.svg", addr.port());
        let err = fetch_image(&ToStub(addr), &url, allow_loopback)
            .await
            .unwrap_err();
        assert!(matches!(err, ImageFetchError::NotImage), "{err:?}");
    }

    #[tokio::test]
    async fn follows_redirects_with_revalidation_and_a_limit() {
        // スタブのポートは bind 後に決まるので、相対 Location で同じスタブへ戻す。
        let (addr, hits) = stub(vec![
            http("302 Found", "Location: /b.png\r\n", b""),
            http("200 OK", "", PNG),
        ])
        .await;
        let url = format!("http://img.example.com:{}/a.png", addr.port());
        let img = fetch_image(&ToStub(addr), &url, allow_loopback)
            .await
            .unwrap();
        assert_eq!(img.mime, "image/png");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "リダイレクト先も取得し直している"
        );

        // 上限を超えるリダイレクトは止める。
        let loops = (0..=MAX_REDIRECTS)
            .map(|_| http("302 Found", "Location: /again\r\n", b""))
            .collect();
        let (addr3, hits3) = stub(loops).await;
        let url = format!("http://img.example.com:{}/a.png", addr3.port());
        let err = fetch_image(&ToStub(addr3), &url, allow_loopback)
            .await
            .unwrap_err();
        assert!(matches!(err, ImageFetchError::TooManyRedirects), "{err:?}");
        assert_eq!(hits3.load(Ordering::SeqCst), MAX_REDIRECTS + 1);
    }

    #[tokio::test]
    async fn redirect_to_a_private_address_is_rejected() {
        // 本番の検証（public_only）では、ループバックに解決される宛先はリダイレクト先でも通らない。
        let (addr, _) = stub(vec![http(
            "302 Found",
            "Location: http://127.0.0.1/x.png\r\n",
            b"",
        )])
        .await;
        let url = format!("http://img.example.com:{}/a.png", addr.port());
        let err = fetch_image(&ToStub(addr), &url, allow_loopback)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ImageFetchError::Rejected(_)),
            "IP 直指定のリダイレクト先は拒否: {err:?}"
        );

        let err = fetch_image(&ToStub(addr), &url, public_fetch::public_only)
            .await
            .unwrap_err();
        assert!(matches!(err, ImageFetchError::Rejected(_)), "{err:?}");
    }

    #[tokio::test]
    async fn rejects_oversized_declared_length() {
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            MAX_IMAGE_BYTES + 1
        );
        let (addr, _) = stub(vec![head.into_bytes()]).await;
        let url = format!("http://img.example.com:{}/big.png", addr.port());
        let err = fetch_image(&ToStub(addr), &url, allow_loopback)
            .await
            .unwrap_err();
        assert!(matches!(err, ImageFetchError::TooLarge), "{err:?}");
    }
}
