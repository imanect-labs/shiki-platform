//! フレーム検証（PIT-35・INV-4「runtime→server 全フレームを server 側が検証」）。
//!
//! script-runtime プロセスは敵対的（サンドボックス由来入力・PIT-23 と同型）として扱う。
//! `HostCall` の全フィールドを、実行に使う前に検証する:
//! サイズ上限・UTF-8/JSON 妥当性・api 名の閉じた集合・seq の単調性・exec_id 一致。

use crate::host::{is_allowed_api, HostCall};

/// ホスト呼び出しリクエスト JSON の上限（script.md §4.3・runtime→server ≤1MB）。
pub const MAX_ARGS_BYTES: usize = 1024 * 1024;

/// フレーム検証エラー（違反は即実行破棄＋監査・INV-4）。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("exec_id 不一致（期待 {expected}, 受信 {got}）")]
    ExecIdMismatch { expected: String, got: String },
    #[error("seq が単調増加でない（前回 {prev}, 受信 {got}）")]
    NonMonotonicSeq { prev: u64, got: u64 },
    #[error("未知の api: {0}")]
    UnknownApi(String),
    #[error("引数が大きすぎます（{0} bytes > {MAX_ARGS_BYTES} bytes）")]
    ArgsTooLarge(usize),
    #[error("不正な引数 JSON: {0}")]
    InvalidArgsJson(String),
    #[error("ホスト呼び出し回数の上限を超えました（{0}）")]
    TooManyCalls(u64),
}

/// フレーム検証器（1 実行 = 1 インスタンス・seq の単調性を跨いで持つ）。
#[derive(Debug)]
pub struct FrameValidator {
    exec_id: String,
    last_seq: Option<u64>,
    call_count: u64,
    max_calls: u64,
}

impl FrameValidator {
    /// 実行 ID と最大呼び出し回数で作る。
    pub fn new(exec_id: impl Into<String>, max_calls: u64) -> Self {
        FrameValidator {
            exec_id: exec_id.into(),
            last_seq: None,
            call_count: 0,
            max_calls,
        }
    }

    /// この検証器が束縛する実行 ID。
    pub fn exec_id(&self) -> &str {
        &self.exec_id
    }

    /// 次に採番すべき seq（`last_seq + 1`・初回は 1）。ホストが seq を採る際に使う。
    pub fn peek_next_seq(&self) -> u64 {
        self.last_seq.map_or(1, |s| s + 1)
    }

    /// 1 フレームを検証し、通れば呼び出しを消費する（seq/回数を前進させる）。
    pub fn check(&mut self, call: &HostCall) -> Result<(), FrameError> {
        if call.exec_id != self.exec_id {
            return Err(FrameError::ExecIdMismatch {
                expected: self.exec_id.clone(),
                got: call.exec_id.clone(),
            });
        }
        if self.call_count >= self.max_calls {
            return Err(FrameError::TooManyCalls(self.max_calls));
        }
        if let Some(prev) = self.last_seq {
            // 厳密に増加（同一 seq の再送・巻き戻しを拒否）。
            if call.seq <= prev {
                return Err(FrameError::NonMonotonicSeq {
                    prev,
                    got: call.seq,
                });
            }
        }
        if !is_allowed_api(&call.api) {
            return Err(FrameError::UnknownApi(call.api.clone()));
        }
        let args_len = serde_json::to_vec(&call.args).map_or(usize::MAX, |v| v.len());
        if args_len > MAX_ARGS_BYTES {
            return Err(FrameError::ArgsTooLarge(args_len));
        }
        self.last_seq = Some(call.seq);
        self.call_count += 1;
        Ok(())
    }
}

/// 生 JSON バイト列を [`HostCall`] へパースしつつ検証する（UTF-8/JSON 妥当性・INV-4 の②③）。
///
/// gRPC の `args_json` は runtime プロセス由来の敵対的入力。ここで初めて構造を信頼する。
pub fn validate_host_call(
    validator: &mut FrameValidator,
    exec_id: &str,
    seq: u64,
    api: &str,
    args_json: &str,
) -> Result<HostCall, FrameError> {
    if args_json.len() > MAX_ARGS_BYTES {
        return Err(FrameError::ArgsTooLarge(args_json.len()));
    }
    // UTF-8 は &str の時点で保証。JSON 妥当性を検証（不正は「未知 api」でなく「不正 JSON」として拒否）。
    let args: serde_json::Value = serde_json::from_str(args_json)
        .map_err(|e| FrameError::InvalidArgsJson(format!("{api}: {e}")))?;
    let call = HostCall {
        exec_id: exec_id.to_string(),
        seq,
        api: api.to_string(),
        args,
    };
    validator.check(&call)?;
    Ok(call)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(exec: &str, seq: u64, api: &str) -> HostCall {
        HostCall {
            exec_id: exec.into(),
            seq,
            api: api.into(),
            args: serde_json::json!({}),
        }
    }

    #[test]
    fn accepts_monotonic_allowed_calls() {
        let mut v = FrameValidator::new("e1", 10);
        assert!(v.check(&call("e1", 1, "storage.read")).is_ok());
        assert!(v.check(&call("e1", 2, "rag.search")).is_ok());
        assert!(v.check(&call("e1", 5, "http.request")).is_ok());
    }

    #[test]
    fn rejects_exec_id_mismatch() {
        let mut v = FrameValidator::new("e1", 10);
        assert!(matches!(
            v.check(&call("evil", 1, "storage.read")),
            Err(FrameError::ExecIdMismatch { .. })
        ));
    }

    #[test]
    fn rejects_non_monotonic_seq() {
        let mut v = FrameValidator::new("e1", 10);
        v.check(&call("e1", 3, "storage.read")).unwrap();
        assert!(matches!(
            v.check(&call("e1", 3, "storage.read")),
            Err(FrameError::NonMonotonicSeq { .. })
        ));
        assert!(matches!(
            v.check(&call("e1", 2, "storage.read")),
            Err(FrameError::NonMonotonicSeq { .. })
        ));
    }

    #[test]
    fn rejects_unknown_api() {
        let mut v = FrameValidator::new("e1", 10);
        assert!(matches!(
            v.check(&call("e1", 1, "secrets.get")),
            Err(FrameError::UnknownApi(_))
        ));
    }

    #[test]
    fn rejects_too_many_calls() {
        let mut v = FrameValidator::new("e1", 2);
        v.check(&call("e1", 1, "log")).unwrap();
        v.check(&call("e1", 2, "log")).unwrap();
        assert!(matches!(
            v.check(&call("e1", 3, "log")),
            Err(FrameError::TooManyCalls(_))
        ));
    }

    #[test]
    fn validate_rejects_bad_json() {
        let mut v = FrameValidator::new("e1", 10);
        assert!(validate_host_call(&mut v, "e1", 1, "storage.read", "{not json").is_err());
    }

    #[test]
    fn validate_parses_good_json() {
        let mut v = FrameValidator::new("e1", 10);
        let c = validate_host_call(&mut v, "e1", 1, "storage.read", "{\"id\":\"x\"}").unwrap();
        assert_eq!(c.args["id"], serde_json::json!("x"));
    }
}
