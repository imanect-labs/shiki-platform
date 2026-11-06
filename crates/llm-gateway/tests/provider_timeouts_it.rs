//! LLM 呼び出しの 3 種のタイムアウトの結線テスト（#554・ローカル axum モック）。
//!
//! 3 アダプタ（Chat Completions / Responses / Anthropic）それぞれで、次の 3 つを確かめる:
//! - 応答ヘッダを返さない → 最初の応答のタイムアウト
//! - 途中で止まる → 無通信のタイムアウト（それまでの差分は届く）
//! - 少しずつ流し続けて全体の上限を超える → 全体のタイムアウト
//! - 受け手がストリームを捨てる → 黙っている上流もタイムアウトを待たずに手放す
//!
//! スタブの `slow:<秒>` の待ちにも同じ上限が掛かることも確かめる。接続できない送り先の誤りに送り先の URL が
//! 入らないことも、同じ 3 アダプタで確かめる（BUG-218）。流し始めた後に接続が切れた読み取りの誤りには、プロバイダが
//! 切った印を付け、HTTP クライアント自体の時間切れには付けないことも確かめる（BUG-224）。
//!
//! どれも `LlmError::Unavailable` で、メッセージにどのタイムアウトかが入る。時間は短く取る。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::response::Response;
use axum::Router;
use futures::stream::{self, StreamExt};

use llm_gateway::model::{GenerateRequest, Message, Role, StreamDelta};
use llm_gateway::provider::{LlmError, LlmProvider};
use llm_gateway::providers::anthropic::AnthropicProvider;
use llm_gateway::providers::openai::OpenAiProvider;
use llm_gateway::providers::openai_responses::OpenAiResponsesProvider;
use llm_gateway::providers::stub::StubProvider;
use llm_gateway::LlmTimeouts;

/// モックの振る舞い。
#[derive(Clone, Copy)]
enum Behavior {
    /// 応答ヘッダを返さない。
    NoHeaders,
    /// 最初の差分を 1 つ流して、以後何も送らない。
    Stall,
    /// 最初の差分のあと、SSE のコメント行を 50ms ごとに流し続ける。
    Trickle,
    /// 400 のヘッダだけ返して、エラー本文を送らない。
    StalledErrorBody,
}

#[derive(Clone, Copy)]
enum Adapter {
    Openai,
    Responses,
    Anthropic,
}

impl Adapter {
    const ALL: [Adapter; 3] = [Adapter::Openai, Adapter::Responses, Adapter::Anthropic];

    /// 本文の差分「Hi」を 1 つ出す SSE イベント。
    fn first_event(self) -> &'static str {
        match self {
            Adapter::Openai => "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n",
            Adapter::Responses => {
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n\n"
            }
            Adapter::Anthropic => {
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n"
            }
        }
    }

    fn provider(self, base: String, timeouts: LlmTimeouts) -> Box<dyn LlmProvider> {
        self.provider_with(reqwest::Client::new(), base, timeouts)
    }

    fn provider_with(
        self,
        http: reqwest::Client,
        base: String,
        timeouts: LlmTimeouts,
    ) -> Box<dyn LlmProvider> {
        match self {
            Adapter::Openai => {
                Box::new(OpenAiProvider::new(http, base, None, "m").with_timeouts(timeouts))
            }
            Adapter::Responses => Box::new(
                OpenAiResponsesProvider::new(http, base, None, "m").with_timeouts(timeouts),
            ),
            Adapter::Anthropic => {
                Box::new(AnthropicProvider::new(http, base, "k", "m").with_timeouts(timeouts))
            }
        }
    }
}

async fn spawn_mock(adapter: Adapter, behavior: Behavior) -> String {
    let first = adapter.first_event();
    let app = Router::new().fallback(move || async move {
        let head = stream::once(async move { Ok::<_, Infallible>(first.to_string()) });
        let body = match behavior {
            Behavior::NoHeaders => {
                tokio::time::sleep(Duration::from_hours(1)).await;
                Body::empty()
            }
            Behavior::Stall => Body::from_stream(head.chain(stream::pending())),
            Behavior::Trickle => Body::from_stream(head.chain(stream::unfold((), |()| async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Some((Ok::<_, Infallible>(": ping\n".to_string()), ()))
            }))),
            Behavior::StalledErrorBody => {
                return Response::builder()
                    .status(400)
                    .body(Body::from_stream(stream::pending::<
                        Result<String, Infallible>,
                    >()))
                    .unwrap();
            }
        };
        Response::builder()
            .header("content-type", "text/event-stream")
            .body(body)
            .unwrap()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn ms(first_response: u64, idle: u64, total: u64) -> LlmTimeouts {
    LlmTimeouts {
        first_response: Duration::from_millis(first_response),
        idle: Duration::from_millis(idle),
        total: Duration::from_millis(total),
    }
}

fn req() -> GenerateRequest {
    GenerateRequest::new(vec![Message::text(Role::User, "hi")])
}

/// ストリームを最初のエラーまで読む（差分・エラーメッセージ）。
async fn drain_until_error(p: &dyn LlmProvider) -> (Vec<StreamDelta>, String) {
    let mut stream = p.stream(&req()).await.expect("headers should arrive");
    let mut deltas = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(d) => deltas.push(d),
            Err(LlmError::Unavailable(m)) => return (deltas, m),
            Err(e) => panic!("expected Unavailable, got {e:?}"),
        }
    }
    panic!("stream ended without a timeout error: {deltas:?}");
}

fn hi() -> StreamDelta {
    StreamDelta::TextDelta { text: "Hi".into() }
}

#[tokio::test]
async fn no_response_headers_hits_first_response_timeout() {
    for adapter in Adapter::ALL {
        let base = spawn_mock(adapter, Behavior::NoHeaders).await;
        let p = adapter.provider(base, ms(200, 5_000, 10_000));
        let started = Instant::now();
        let Err(LlmError::Unavailable(msg)) = p.stream(&req()).await.map(|_| ()) else {
            panic!("{}: expected Unavailable", p.name());
        };
        assert!(msg.contains("first-response timeout"), "{msg}");
        assert!(started.elapsed() < Duration::from_secs(3), "{}", p.name());
    }
}

/// 接続できない送り先の誤りは、送り先の URL（ホスト・ポート・パス・base_url の userinfo）を入れず、誤りの種類だけを
/// 言う（BUG-218。reqwest の文は `error sending request for url (…)`）。
#[tokio::test]
async fn unreachable_providers_are_not_named_in_the_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    for adapter in Adapter::ALL {
        let p = adapter.provider(
            format!("http://user:pw-secret@{addr}/base"),
            ms(5_000, 5_000, 10_000),
        );
        let Err(LlmError::Unavailable(msg)) = p.stream(&req()).await.map(|_| ()) else {
            panic!("{}: expected Unavailable", p.name());
        };
        let port = addr.port().to_string();
        for leaked in ["127.0.0.1", port.as_str(), "pw-secret", "/base", "http"] {
            assert!(!msg.contains(leaked), "{leaked} が漏れた: {msg}");
        }
        assert!(msg.ends_with(": プロバイダに接続できませんでした"), "{msg}");
    }
}

#[tokio::test]
async fn stalled_error_body_is_a_timeout_not_a_bad_request() {
    for adapter in Adapter::ALL {
        let base = spawn_mock(adapter, Behavior::StalledErrorBody).await;
        let p = adapter.provider(base, ms(5_000, 200, 10_000));
        // 400 でも本文が止まったならタイムアウト（再試行・フォールバックの対象）として返す。
        match p.stream(&req()).await.map(|_| ()) {
            Err(LlmError::Unavailable(msg)) => assert!(msg.contains("idle timeout"), "{msg}"),
            other => panic!("{}: expected Unavailable, got {other:?}", p.name()),
        }
    }
}

#[tokio::test]
async fn stalled_stream_hits_idle_timeout_after_partial_output() {
    for adapter in Adapter::ALL {
        let base = spawn_mock(adapter, Behavior::Stall).await;
        // 無通信の窓は CI の遅延で最初の差分を取りこぼさない程度に取る。
        let p = adapter.provider(base, ms(5_000, 1_000, 10_000));
        let started = Instant::now();
        let (deltas, msg) = drain_until_error(p.as_ref()).await;
        assert_eq!(deltas, vec![hi()], "{}", p.name());
        assert!(msg.contains("idle timeout"), "{msg}");
        assert!(started.elapsed() < Duration::from_secs(4), "{}", p.name());
    }
}

#[tokio::test]
async fn trickling_stream_hits_total_timeout() {
    for adapter in Adapter::ALL {
        let base = spawn_mock(adapter, Behavior::Trickle).await;
        // 50ms ごとに届くので無通信（500ms）には掛からず、全体（600ms）で切れる。
        let p = adapter.provider(base, ms(5_000, 500, 600));
        let started = Instant::now();
        let (deltas, msg) = drain_until_error(p.as_ref()).await;
        assert_eq!(deltas, vec![hi()], "{}", p.name());
        assert!(msg.contains("total timeout"), "{msg}");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(500) && elapsed < Duration::from_secs(3),
            "{}: {elapsed:?}",
            p.name()
        );
    }
}

/// 本文が捨てられたら立つ旗（モック側で上流の接続が閉じたことを見る）。
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// 最初の差分を流して黙るモック。本文（＝接続）が捨てられたら旗が立つ。
async fn spawn_stall_with_flag(adapter: Adapter) -> (String, Arc<AtomicBool>) {
    let dropped = Arc::new(AtomicBool::new(false));
    let flag = dropped.clone();
    let first = adapter.first_event();
    let app = Router::new().fallback(move || {
        let guard = DropFlag(flag.clone());
        async move {
            let head = stream::once(async move { Ok::<_, Infallible>(first.to_string()) });
            let body = head.chain(stream::pending()).map(move |chunk| {
                let _keep = &guard;
                chunk
            });
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Body::from_stream(body))
                .unwrap()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), dropped)
}

#[tokio::test]
async fn dropping_the_stream_releases_a_silent_upstream_before_any_timeout() {
    for adapter in Adapter::ALL {
        let (base, dropped) = spawn_stall_with_flag(adapter).await;
        // 無通信・全体とも長い。受け手が閉じたら、それを待たずに上流を手放す。
        let p = adapter.provider(base, ms(5_000, 60_000, 120_000));
        let mut stream = p.stream(&req()).await.expect("headers should arrive");
        assert_eq!(stream.next().await.unwrap().unwrap(), hi(), "{}", p.name());
        drop(stream);
        let started = Instant::now();
        while !dropped.load(Ordering::SeqCst) {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "{}: upstream still held",
                p.name()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// スタブの `slow:<秒>`（利用者の入力で決まる待ち）にも同じ上限が掛かる。
#[tokio::test]
async fn stub_slow_hold_is_capped_by_the_same_timeouts() {
    let slow = |secs: u64| {
        GenerateRequest::new(vec![Message::text(
            Role::User,
            format!("slow:{secs} こんにちは"),
        )])
    };
    let drain = |p: StubProvider, r: GenerateRequest| async move {
        let mut stream = p.stream(&r).await.unwrap();
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            out.push(item);
        }
        out
    };

    // 無通信の上限（200ms）を超える待ちは、本文のあと idle timeout で切れる。
    let started = Instant::now();
    let out = drain(
        StubProvider::new().with_timeouts(ms(5_000, 200, 10_000)),
        slow(3600),
    )
    .await;
    assert!(matches!(&out[0], Ok(StreamDelta::TextDelta { .. })));
    assert!(
        matches!(&out[1], Err(LlmError::Unavailable(m)) if m.contains("idle timeout")),
        "{out:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(3));

    // 全体の上限の方が短ければ total timeout。
    let out = drain(
        StubProvider::new().with_timeouts(ms(5_000, 5_000, 200)),
        slow(3600),
    )
    .await;
    assert!(
        matches!(&out[1], Err(LlmError::Unavailable(m)) if m.contains("total timeout")),
        "{out:?}"
    );

    // 上限内の待ちはそのまま完了する。
    let out = drain(
        StubProvider::new().with_timeouts(ms(5_000, 5_000, 10_000)),
        slow(0),
    )
    .await;
    assert!(
        matches!(out.last(), Some(Ok(StreamDelta::Done { .. }))),
        "{out:?}"
    );
}

/// 最初の差分をチャンクで 1 つ送り、終わりのチャンクを送らないまま接続を閉じる送り先（応答の途中で接続が切れる）。
async fn spawn_cut_connection(adapter: Adapter) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let first = adapter.first_event();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            // 要求をヘッダと content-length の本文まで読み切ってから答える。
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = sock.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
                let len: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                if buf.len() >= end + 4 + len {
                    break;
                }
            }
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n{:x}\r\n{first}\r\n",
                first.len()
            );
            let _ = sock.write_all(reply.as_bytes()).await;
            let _ = sock.flush().await;
            // 終わりのチャンク（0）を送らずに閉じる。
            drop(sock);
        }
    });
    format!("http://{addr}")
}

/// 流し始めた後に接続が切れた読み取りの誤りは、プロバイダが切った印の付いた一時障害にする（まだ何も確定していなければ
/// gateway が送り直す・BUG-224）。3 アダプタに共通の読み取り（`timeouts.rs` の `next_chunk`）を通る。
#[tokio::test]
async fn a_connection_cut_midway_is_marked_as_cut_by_the_provider() {
    for adapter in Adapter::ALL {
        let base = spawn_cut_connection(adapter).await;
        let p = adapter.provider(base, ms(5_000, 5_000, 10_000));
        let (deltas, msg) = drain_until_error(p.as_ref()).await;
        assert_eq!(deltas, vec![hi()], "{}", p.name());
        assert!(
            msg.starts_with("cut by provider: stream: "),
            "{}: {msg}",
            p.name()
        );
        assert!(!msg.contains("127.0.0.1"), "{msg}");
    }
}

/// HTTP クライアント自体の時間切れ（本文の途中）は暴走止めなので、送り直しの印を付けない（BUG-224）。
#[tokio::test]
async fn the_http_clients_own_timeout_is_not_marked_as_cut() {
    for adapter in Adapter::ALL {
        let base = spawn_mock(adapter, Behavior::Stall).await;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(800))
            .build()
            .unwrap();
        let p = adapter.provider_with(http, base, ms(5_000, 5_000, 10_000));
        let (deltas, msg) = drain_until_error(p.as_ref()).await;
        assert_eq!(deltas, vec![hi()], "{}", p.name());
        assert!(msg.starts_with("stream: "), "{}: {msg}", p.name());
    }
}
