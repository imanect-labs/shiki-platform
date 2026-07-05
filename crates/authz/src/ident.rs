//! 識別子の文字ポリシー単一定義（SAAS.1 / #91）。
//!
//! tenant_id・FGA local id の禁止文字判定はここだけに置く。呼び出し側
//! （`crates/api` の `resolve_tenant_id` / admin ルート / `shiki-admin` CLI、
//! `crates/storage` の共有先検証、role claim 正規化）は本モジュールへ委譲し、
//! 手書きの文字集合を再定義しない（codegen を正とする不変条件のバリデーション版。
//! 判定が分散すると集合が drift し、CLI だけ `/` を許す等の穴が生まれる）。

use crate::object::TENANT_SEP;

/// 識別子ポリシー違反。呼び出し側が各層のエラー型（`ApiError` / `StorageError` 等）へ写す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentViolation {
    /// 空文字列。
    Empty,
    /// 禁止文字を含む（`|` = TENANT_SEP・FGA 構造文字・パス境界など）。
    ForbiddenChar(char),
    /// 空白文字を含む。
    Whitespace,
    /// 予約名（`.` / `..`）。
    Reserved,
}

impl std::fmt::Display for IdentViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IdentViolation::Empty => f.write_str("空です"),
            IdentViolation::ForbiddenChar(c) => write!(f, "禁止文字 {c:?} を含みます"),
            IdentViolation::Whitespace => f.write_str("空白文字を含みます"),
            IdentViolation::Reserved => f.write_str("予約名（. / ..）です"),
        }
    }
}

impl std::error::Error for IdentViolation {}

/// `tenant_id` の検証（fail-closed）。
///
/// tenant_id は FGA 識別子 `<type>:<tenant_id>|<local_id>`（[`TENANT_SEP`]）と
/// オブジェクトキー `{tenant_id}/{org}/...` の両方の名前空間になるため、
/// - `|`: 名前空間パースを曖昧化（越境の余地）
/// - `:` `#` `@`: FGA の型/userset/構造区切り
/// - `/`: オブジェクトキーの prefix 境界を破壊（purge/list の越境）
/// - 空白・制御文字: 事故と難読化の温床
/// - `.` `..`: オブジェクトキーのセグメントとして紛らわしい予約名
///
/// を全て拒否する。claim・CLI 引数・API 入力のいずれ由来でも信頼せず本関数を通すこと。
pub fn validate_tenant_id(tenant_id: &str) -> Result<(), IdentViolation> {
    if tenant_id.is_empty() {
        return Err(IdentViolation::Empty);
    }
    if tenant_id == "." || tenant_id == ".." {
        return Err(IdentViolation::Reserved);
    }
    const FORBIDDEN: &[char] = &[TENANT_SEP, ':', '#', '@', '/'];
    for c in tenant_id.chars() {
        if c.is_whitespace() {
            return Err(IdentViolation::Whitespace);
        }
        if c.is_control() {
            return Err(IdentViolation::ForbiddenChar(c));
        }
        if FORBIDDEN.contains(&c) {
            return Err(IdentViolation::ForbiddenChar(c));
        }
    }
    Ok(())
}

/// FGA local id（共有先 user/role id・role claim 由来の role id 等）の検証。
///
/// local id は `<type>:<tenant>|<local>` の `<local>` に入るため、型区切り `:`・
/// userset 区切り `#`・tenant 区切り `|`（[`TENANT_SEP`]）と制御文字を拒否する。
/// AD group パス由来の `/` や email 由来の `@` は正当な local id として**許可**する
/// （tenant prefix が先頭に付くため名前空間は壊れない）。
pub fn validate_local_id(id: &str) -> Result<(), IdentViolation> {
    if id.is_empty() {
        return Err(IdentViolation::Empty);
    }
    for c in id.chars() {
        if c == ':' || c == '#' || c == TENANT_SEP {
            return Err(IdentViolation::ForbiddenChar(c));
        }
        if c.is_control() {
            return Err(IdentViolation::ForbiddenChar(c));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_id_accepts_normal_slugs() {
        for ok in ["default", "acme", "a-corp_1.eu", "t1"] {
            assert_eq!(validate_tenant_id(ok), Ok(()), "{ok:?} は許可されること");
        }
    }

    #[test]
    fn tenant_id_rejects_structural_chars() {
        // FGA 区切り・構造文字・オブジェクトキー境界。
        for bad in ["ac|me", "ac:me", "ac#me", "ac@me", "a/b"] {
            assert!(
                matches!(
                    validate_tenant_id(bad),
                    Err(IdentViolation::ForbiddenChar(_))
                ),
                "{bad:?} は拒否されること"
            );
        }
    }

    #[test]
    fn tenant_id_rejects_whitespace_empty_reserved() {
        assert_eq!(validate_tenant_id("ac me"), Err(IdentViolation::Whitespace));
        assert_eq!(
            validate_tenant_id("ac\tme"),
            Err(IdentViolation::Whitespace)
        );
        assert_eq!(validate_tenant_id(""), Err(IdentViolation::Empty));
        // `.` / `..` はオブジェクトキーのセグメントとして紛らわしいため予約（#91 L-4）。
        assert_eq!(validate_tenant_id("."), Err(IdentViolation::Reserved));
        assert_eq!(validate_tenant_id(".."), Err(IdentViolation::Reserved));
        // 制御文字。
        assert!(matches!(
            validate_tenant_id("a\u{7}b"),
            Err(IdentViolation::ForbiddenChar(_))
        ));
    }

    #[test]
    fn local_id_allows_ad_paths_and_emails() {
        // `/`（AD group パス）と `@`（email 由来 username）は local id として正当。
        for ok in ["sales/team-1", "alice@example.com", "dept-1", "役員"] {
            assert_eq!(validate_local_id(ok), Ok(()), "{ok:?} は許可されること");
        }
    }

    #[test]
    fn local_id_rejects_fga_structural_chars() {
        for bad in ["a:b", "c#d", "e|f"] {
            assert!(
                matches!(
                    validate_local_id(bad),
                    Err(IdentViolation::ForbiddenChar(_))
                ),
                "{bad:?} は拒否されること"
            );
        }
        assert_eq!(validate_local_id(""), Err(IdentViolation::Empty));
        assert!(matches!(
            validate_local_id("a\nb"),
            Err(IdentViolation::ForbiddenChar(_))
        ));
    }
}
