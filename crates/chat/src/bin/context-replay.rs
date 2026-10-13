//! 記録済みスレッドでコンテキスト剪定を再生し、条件ごとのレポートを JSON Lines で出す（#532）。
//!
//! 入力は `GET /threads/{id}/messages` の応答 JSON（認可済み API からのエクスポート）。
//!
//! ```text
//! cargo run -p shiki-chat --bin context-replay -- --input thread.json \
//!     [--message <assistant message uuid>] [--window 32000,64000,none] [--soft 0,24000] \
//!     [--keep-recent 6] [--max-tokens 4096] [--overhead 6000] [--history-prefix-tokens 0]
//! ```
//!
//! `--history-prefix-tokens` は記録に残らない先頭履歴（ピン留め skill の few-shot 等）のトークン数。
//! 剪定はツール結果しか畳まないので、テキスト履歴の効果はトークン数だけで決まる。同じ量の詰め物を
//! 先頭に置けば剪定のタイミングを再現できる。
//!
//! `--soft 0` は管理なし（ベースライン）。窓 × 設定値の全組み合わせを 1 行ずつ出す。
//! 自律プロファイルの run を前提にする（Chat の run は本番で剪定しない）。見積りの限界は
//! `agent_core::context_replay` のモジュール doc を参照。

// CLI バイナリ: 標準出力/標準エラーへの出力は正当な用途のため print 系 lint を許容する。
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::process::ExitCode;

use agent_core::context_replay::{replay, ReplayConfig};
use chat::model::Message;
use chat::worker::trajectory::from_messages;

/// 応答 JSON のうち再生に要る部分（`MessagesResponse`）。
///
/// 応答オブジェクトだけを受ける。素の `messages` 配列は run が進行中かどうかを持たないので、
/// 未完の軌跡を完了済みとして再生しかねない。
#[derive(serde::Deserialize)]
struct Export {
    messages: Vec<Message>,
    /// 進行中 run の assistant メッセージ（あれば未完の軌跡なので再生しない）。
    #[serde(default)]
    active_assistant_message_id: Option<uuid::Uuid>,
}

struct Args {
    input: String,
    message: Option<uuid::Uuid>,
    windows: Vec<Option<u32>>,
    softs: Vec<usize>,
    keep_recent: usize,
    max_tokens: u32,
    overhead: usize,
    history_prefix_tokens: usize,
}

fn parse_list<T>(v: &str, f: impl Fn(&str) -> Result<T, String>) -> Result<Vec<T>, String> {
    v.split(',').map(|s| f(s.trim())).collect()
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        input: String::new(),
        message: None,
        windows: vec![Some(32_000), Some(64_000), Some(128_000)],
        softs: vec![0, 24_000],
        keep_recent: 6,
        max_tokens: 4_096,
        overhead: 6_000,
        history_prefix_tokens: 0,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it
            .next()
            .ok_or_else(|| format!("{flag} に値がありません"))?;
        let num = |s: &str| s.parse::<usize>().map_err(|e| format!("{flag}: {s}: {e}"));
        match flag.as_str() {
            "--input" => args.input = value,
            "--message" => {
                args.message = Some(value.parse().map_err(|e| format!("--message: {e}"))?);
            }
            "--window" => {
                args.windows = parse_list(&value, |s| {
                    if s == "none" {
                        Ok(None)
                    } else {
                        s.parse()
                            .map(Some)
                            .map_err(|e| format!("--window: {s}: {e}"))
                    }
                })?;
            }
            "--soft" => args.softs = parse_list(&value, num)?,
            "--keep-recent" => args.keep_recent = num(&value)?,
            "--max-tokens" => {
                args.max_tokens = value.parse().map_err(|e| format!("--max-tokens: {e}"))?;
            }
            "--overhead" => args.overhead = num(&value)?,
            "--history-prefix-tokens" => args.history_prefix_tokens = num(&value)?,
            other => return Err(format!("不明な引数: {other}")),
        }
    }
    if args.input.is_empty() {
        return Err("--input <thread.json> が必要です".into());
    }
    Ok(args)
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let raw = std::fs::read_to_string(&args.input).map_err(|e| format!("{}: {e}", args.input))?;
    let Export {
        messages,
        active_assistant_message_id: active,
    } = serde_json::from_str(&raw)
        .map_err(|e| format!("GET /threads/{{id}}/messages の応答オブジェクトを渡すこと: {e}"))?;
    let mut trajectory = from_messages(&messages, args.message).map_err(|e| e.to_string())?;
    if active == Some(trajectory.message_id) {
        return Err(format!(
            "message {} の run はまだ実行中です（完了後にエクスポートし直すこと）",
            trajectory.message_id
        ));
    }
    if args.history_prefix_tokens > 0 {
        // bytes/4 でも ASCII の見積りでも同じトークン数になる詰め物（剪定はテキストを畳まない）。
        trajectory.initial.insert(
            0,
            llm_gateway::Message::text(
                llm_gateway::Role::User,
                " ".repeat(args.history_prefix_tokens * 4),
            ),
        );
    }
    eprintln!(
        "message {}: {} ステップを再生（前ターン {} 件）",
        trajectory.message_id,
        trajectory.steps.len(),
        trajectory.initial.len()
    );
    eprintln!(
        "注意: 自律プロファイルの run として再生します（Chat の run は本番では剪定されません）"
    );
    for &window_tokens in &args.windows {
        for &soft_limit_tokens in &args.softs {
            let report = replay(
                &trajectory.initial,
                &trajectory.steps,
                ReplayConfig {
                    soft_limit_tokens,
                    keep_recent: args.keep_recent,
                    window_tokens,
                    max_output_tokens: Some(args.max_tokens),
                    overhead_tokens: args.overhead,
                },
            );
            println!(
                "{}",
                serde_json::to_string(&report).map_err(|e| e.to_string())?
            );
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("context-replay: {e}");
            ExitCode::FAILURE
        }
    }
}
