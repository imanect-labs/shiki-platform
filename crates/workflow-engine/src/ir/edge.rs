//! エッジ（ノード間接続・ir.md §5）。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// 1 エッジ（`from` ノードの `from_port` → `to` ノード）。
///
/// join 以外の入エッジは 1 本制約（V2）。エッジ状態は永続化せず taken_ports から導出（engine.md §2.3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct Edge {
    /// 出力元ノード id。
    pub from: String,
    /// 出力ポート名（省略時 `out`。branch は `true`/`false`、switch は case 名、error は `error`）。
    #[serde(default = "default_port")]
    pub from_port: String,
    /// 入力先ノード id。
    pub to: String,
}

fn default_port() -> String {
    "out".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn edge_defaults_out_port() {
        let e: Edge = serde_json::from_value(json!({"from": "a", "to": "b"})).unwrap();
        assert_eq!(e.from_port, "out");
    }

    #[test]
    fn edge_explicit_port() {
        let e: Edge =
            serde_json::from_value(json!({"from": "cond", "from_port": "true", "to": "b"}))
                .unwrap();
        assert_eq!(e.from_port, "true");
    }

    #[test]
    fn edge_deny_unknown() {
        let bad: Result<Edge, _> = serde_json::from_value(json!({"from": "a", "to": "b", "x": 1}));
        assert!(bad.is_err());
    }
}
