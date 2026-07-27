//! OpenAI 互換 API の function 名マッピング（`^[a-zA-Z0-9_-]{1,64}$`・#352）。
//!
//! アプリ側の語彙（`office.live_edit` 等のドット付きツール名）は一切変えず、**送信時だけ**
//! ワイヤ名へ写して応答で逆写しする（codegen が正・語彙の二重定義を作らない）。

use std::collections::BTreeMap;

/// OpenAI 互換の function 名制約（`^[a-zA-Z0-9_-]{1,64}$`）への写像。
///
/// shiki のツール名は `office.live_edit` のようにドットを含むが、DeepSeek 等の厳格な
/// プロバイダは違反名を 400 で拒否する。送信時にワイヤ名へ写し、応答のツール呼び出しで
/// 元名へ逆写しする（アプリ側の語彙は一切変えない）。
/// OpenAI 互換 API の function 名の上限（`^[a-zA-Z0-9_-]{1,64}$`）。
const WIRE_NAME_MAX: usize = 64;

pub(super) struct ToolNameMap {
    to_wire: BTreeMap<String, String>,
    from_wire: BTreeMap<String, String>,
}

impl ToolNameMap {
    pub(super) fn new<'a>(names: impl Iterator<Item = &'a str>) -> Self {
        let mut to_wire = BTreeMap::new();
        let mut from_wire: BTreeMap<String, String> = BTreeMap::new();
        for name in names {
            let mut wire = sanitize_tool_name(name);
            // 衝突（別名が同一ワイヤ名へ潰れた場合）は接尾辞で一意化する。
            //
            // 接尾辞の分だけ**先に**切り詰める。そうしないとサニタイズ後がちょうど 64 文字の
            // 別名同士で `truncate(64)` が接尾辞を削り落とし、同じ wire を作り続けて無限ループする。
            let mut n = 2;
            while from_wire.get(&wire).is_some_and(|orig| orig != name) {
                let suffix = format!("_{n}");
                let head_len = WIRE_NAME_MAX.saturating_sub(suffix.len());
                let mut head = sanitize_tool_name(name);
                head.truncate(head_len);
                wire = format!("{head}{suffix}");
                n += 1;
            }
            from_wire.insert(wire.clone(), name.to_string());
            to_wire.insert(name.to_string(), wire);
        }
        ToolNameMap { to_wire, from_wire }
    }

    /// 元名 → ワイヤ名（未登録＝履歴にだけ現れる過去ツール等はその場でサニタイズ）。
    pub(super) fn wire(&self, original: &str) -> String {
        self.to_wire
            .get(original)
            .cloned()
            .unwrap_or_else(|| sanitize_tool_name(original))
    }

    /// ワイヤ名 → 元名（未登録はそのまま返す＝素通し）。
    pub(super) fn original(&self, wire: &str) -> String {
        self.from_wire
            .get(wire)
            .cloned()
            .unwrap_or_else(|| wire.to_string())
    }
}

/// 許可外の文字を `_` に置換し 64 文字へ丸める。
fn sanitize_tool_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    s.truncate(WIRE_NAME_MAX);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// サニタイズ後が **64 文字ちょうど** の別名同士でも衝突解決が停止する（旧実装は無限ループ）。
    #[test]
    fn collision_with_max_length_names_terminates() {
        let a = format!("{}.x", "a".repeat(63)); // サニタイズ後 64 文字
        let b = format!("{}_x", "a".repeat(63)); // 同じ 64 文字へ潰れる
        let map = ToolNameMap::new([a.as_str(), b.as_str()].into_iter());
        let wa = map.wire(&a);
        let wb = map.wire(&b);
        assert_ne!(wa, wb, "衝突が解消されていない");
        for w in [&wa, &wb] {
            assert!(w.len() <= 64, "{w} が 64 文字を超えた");
            assert!(w
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
        }
        assert_eq!(map.original(&wa), a);
        assert_eq!(map.original(&wb), b);
    }

    #[test]
    fn colliding_sanitized_names_get_unique_suffix() {
        let names = ToolNameMap::new(["a.b", "a_b"].into_iter());
        // 先着が a_b を取り、後発は接尾辞で一意化される（逆写しはどちらも正しい）。
        let wire1 = names.wire("a.b");
        let wire2 = names.wire("a_b");
        assert_ne!(wire1, wire2);
        assert_eq!(names.original(&wire1), "a.b");
        assert_eq!(names.original(&wire2), "a_b");
    }
}
