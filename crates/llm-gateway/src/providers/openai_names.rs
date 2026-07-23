//! OpenAI 互換 wire へのツール名写像（`openai.rs` から分割・500 行規約）。
//!
//! OpenAI の function.name 制約は `^[a-zA-Z0-9_-]{1,64}$`。このリポジトリのツール名には
//! ドット入り（`document.edit` 等）があり、寛容なプロバイダは通すが DeepSeek 等は 400 で
//! 拒否する。送信時に許容外文字を `_` へ写して 64 文字へ切り詰め、受信時は
//! [`ToolNameMap`] で元の名前へ逆写しする。

use std::collections::HashMap;

/// OpenAI の function.name 制約へ写した wire 名（単体では衝突を解決しない）。
fn sanitize_wire_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

/// ツール名の双方向写像（ローカル名 ⇄ wire 名）。リクエストの tools ＋履歴中の ToolUse 名から
/// 構築し、サニタイズ衝突（`a.b` と `a_b` の併存等）は接尾辞で一意化して往復可能に保つ。
pub(super) struct ToolNameMap {
    to_wire: HashMap<String, String>,
    to_local: HashMap<String, String>,
}

impl ToolNameMap {
    pub(super) fn new(names: impl Iterator<Item = impl AsRef<str>>) -> Self {
        let mut to_wire = HashMap::new();
        let mut to_local: HashMap<String, String> = HashMap::new();
        for name in names {
            let local = name.as_ref().to_string();
            if to_wire.contains_key(&local) {
                continue;
            }
            let mut wire = sanitize_wire_name(&local);
            let mut n = 2;
            while to_local.contains_key(&wire) {
                // 接尾辞を足しても 64 文字制約を守る（ベース側を切り詰める）。
                // sanitize 済みなので ASCII のみ＝バイト位置での切り詰めが安全。
                let suffix = format!("_{n}");
                let base = sanitize_wire_name(&local);
                let keep = base.len().min(64usize.saturating_sub(suffix.len()));
                wire = format!("{}{suffix}", &base[..keep]);
                n += 1;
            }
            to_local.insert(wire.clone(), local.clone());
            to_wire.insert(local, wire);
        }
        ToolNameMap { to_wire, to_local }
    }

    pub(super) fn wire(&self, local: &str) -> String {
        self.to_wire
            .get(local)
            .cloned()
            .unwrap_or_else(|| sanitize_wire_name(local))
    }

    pub(super) fn local(&self, wire: &str) -> String {
        self.to_local
            .get(wire)
            .cloned()
            .unwrap_or_else(|| wire.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_names_round_trip() {
        let names = ToolNameMap::new(["document.edit", "csv.write", "fs_write"].into_iter());
        assert_eq!(names.wire("document.edit"), "document_edit");
        assert_eq!(names.wire("csv.write"), "csv_write");
        assert_eq!(names.wire("fs_write"), "fs_write");
        assert_eq!(names.local("document_edit"), "document.edit");
        assert_eq!(names.local("csv_write"), "csv.write");
    }

    #[test]
    fn sanitize_collision_is_disambiguated() {
        // `a.b` と `a_b` が併存してもサニタイズ後に衝突せず往復可能。
        let names = ToolNameMap::new(["a.b", "a_b"].into_iter());
        let w1 = names.wire("a.b");
        let w2 = names.wire("a_b");
        assert_ne!(w1, w2);
        assert_eq!(names.local(&w1), "a.b");
        assert_eq!(names.local(&w2), "a_b");
    }

    #[test]
    fn collision_suffix_respects_64_char_limit() {
        // 64 文字ちょうどで衝突する名前でも、接尾辞込みで 64 文字以内に収まる。
        let long_dot = format!("{}.x", "a".repeat(62)); // sanitize 後 64 文字
        let long_us = format!("{}_x", "a".repeat(62)); // 既に 64 文字・同じ wire 名に写る
        let names = ToolNameMap::new([long_dot.as_str(), long_us.as_str()].into_iter());
        let w1 = names.wire(&long_dot);
        let w2 = names.wire(&long_us);
        assert_ne!(w1, w2, "衝突が解消される");
        assert!(
            w1.len() <= 64 && w2.len() <= 64,
            "64 文字制約を維持: {w1} / {w2}"
        );
        assert_eq!(names.local(&w1), long_dot);
        assert_eq!(names.local(&w2), long_us);
    }
}
