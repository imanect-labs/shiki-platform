//! 能力呼び出しの委譲窓口（INV-1: runtime は資格情報を持たない・INV-2: 能力ゲートウェイ一元）。
//!
//! ゲスト（QuickJS）が発した `Shiki.*` は 1 本のホスト呼び出しへ集約される。runtime は
//! 「どの api をどのペイロードで呼びたいか」を [`HostCall`] で伝えるだけで、実際の認可・実行・
//! 監査は [`HostCallHandler`] の実装（= shiki-server / workflow-engine の能力ゲートウェイ）が担う。

use async_trait::async_trait;

/// ゲストからの能力呼び出し（フレーム検証済み・script.md §5）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCall {
    /// 実行 ID（フレーム照合）。
    pub exec_id: String,
    /// 実行内で単調増加する連番（冪等キー派生・engine.md §7.3）。
    pub seq: u64,
    /// 能力 api 名（閉じた集合・[`ALLOWED_APIS`] に照合済み）。
    pub api: String,
    /// 引数 JSON。
    pub args: serde_json::Value,
}

/// 能力呼び出しの応答（ゲストへ返す）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostResponse {
    /// 成功。値をゲストへ返す。
    Ok(serde_json::Value),
    /// 失敗。ゲスト側で `ShikiError` として throw される。
    Err {
        message: String,
        code: String,
        retryable: bool,
    },
}

impl HostResponse {
    /// ゲストが `JSON.parse` する応答エンベロープへ直列化する。
    pub fn to_envelope(&self) -> serde_json::Value {
        match self {
            HostResponse::Ok(v) => serde_json::json!({ "ok": true, "value": v }),
            HostResponse::Err {
                message,
                code,
                retryable,
            } => serde_json::json!({
                "ok": false,
                "error": { "message": message, "code": code, "retryable": retryable }
            }),
        }
    }
}

/// 能力呼び出しの実処理（認可・監査・実行）を担う委譲先。
///
/// runtime プロセスはこのトレイトを **実装しない**（資格情報を持たないため）。
/// gRPC の `HostCall` フレームを受けた shiki-server 側がこの実装へ橋渡しする。
#[async_trait]
pub trait HostCallHandler: Send + Sync {
    /// 1 件の能力呼び出しを処理する。エラーは `HostResponse::Err` で表現し、
    /// トレイト自体は基本的に失敗しない（内部エラーも Err 応答へ写す）。
    async fn handle(&self, call: &HostCall) -> HostResponse;
}

/// runtime が受理する能力 api の閉じた集合（Stage A・script.md §6）。
///
/// Stage B で `data.*` / `notify.send` を追加する（能力面のみ・9.2/9.10 後）。
pub const ALLOWED_APIS: &[&str] = &[
    "storage.read",
    "storage.list",
    "storage.write",
    "rag.search",
    "http.request",
    "workflow.start",
    "log",
    "context",
];

/// api 名が閉じた集合に属するか。
pub fn is_allowed_api(api: &str) -> bool {
    ALLOWED_APIS.contains(&api)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_shapes() {
        let ok = HostResponse::Ok(serde_json::json!({ "n": 1 }));
        assert_eq!(ok.to_envelope()["ok"], serde_json::json!(true));
        let err = HostResponse::Err {
            message: "boom".into(),
            code: "permanent".into(),
            retryable: false,
        };
        let e = err.to_envelope();
        assert_eq!(e["ok"], serde_json::json!(false));
        assert_eq!(e["error"]["message"], serde_json::json!("boom"));
    }

    #[test]
    fn allowed_apis_closed_set() {
        assert!(is_allowed_api("storage.read"));
        assert!(is_allowed_api("workflow.start"));
        // data.* は Stage B（現時点では拒否）。
        assert!(!is_allowed_api("data.query"));
        assert!(!is_allowed_api("secrets.get"));
        assert!(!is_allowed_api("../escape"));
    }
}
