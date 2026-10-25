//! ユーザー・部署（`user_ref` / `role_ref`）の列の値の **ID の形式**（単一定義）。
//!
//! 書込の検証（[`crate::validate`]）と絞り込みの比較値の検査（[`crate::query::ref_operands`]）が同じ形式を使う。
//! 書込はこの形式に加えてディレクトリに居ることを確かめるが、絞り込みは**形式だけ**を確かめる（退職者など
//! ディレクトリから消えた人の ID で過去の行を絞り込めるように）。
//!
//! - ユーザー ID は IdP の `sub`（OIDC Core: ASCII・255 文字以内）。空白・制御文字・非 ASCII を含まない。
//!   表示名（「山田 太郎」「Taro Yamada」）はこの形式に合わない
//! - 部署（ロール）ID はログイン時の roles / groups クレームから作る（前後の空白を落とし、FGA の構造文字
//!   `:` `#` `|` と制御文字を含むものは捨てる・[`authz::validate_local_id`]）。部署のパス（`acme/営業 第1部`）は
//!   空白や非 ASCII を含み得るので、部署の表示名と ID は形式だけでは区別できない

/// ID の長さの上限（バイト・書込の検証の上限と同じ）。
pub(crate) const MAX_REF_ID_BYTES: usize = 256;

/// ユーザー ID の形式か（空でない・上限以内・印字可能な ASCII だけ）。
pub(crate) fn is_user_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_REF_ID_BYTES && s.bytes().all(|b| b.is_ascii_graphic())
}

/// 部署（ロール）ID の形式か（空でない・上限以内・前後に空白が無い・FGA の local id として正しい）。
pub(crate) fn is_role_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_REF_ID_BYTES
        && s.trim() == s
        && authz::validate_local_id(s).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_ids_are_printable_ascii() {
        for ok in [
            "alice",
            "u7",
            "00000000-0000-0000-0000-000000000001",
            "auth0|abc",
            "a@example.com",
        ] {
            assert!(is_user_id(ok), "{ok:?}");
        }
        let long = "a".repeat(MAX_REF_ID_BYTES + 1);
        for bad in [
            "",
            "山田 太郎",
            "山田太郎",
            "Taro Yamada",
            " alice",
            "a\tb",
            &long,
        ] {
            assert!(!is_user_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn role_ids_follow_the_login_claim_rules() {
        for ok in ["sales", "acme/eng", "acme/営業 第1部", "営業部"] {
            assert!(is_role_id(ok), "{ok:?}");
        }
        let long = "r".repeat(MAX_REF_ID_BYTES + 1);
        for bad in [
            "",
            " sales",
            "sales ",
            "role:sales",
            "a#b",
            "t|r",
            "a\nb",
            &long,
        ] {
            assert!(!is_role_id(bad), "{bad:?}");
        }
    }
}
