//! `shiki-admin retenant` のオブジェクトキー写像（`blob.object_key` の名前空間差し替え）。
//!
//! 本体（`shiki-admin.rs`）が 500 行ゲートに張り付いていたため、純粋関数とその単体テストを
//! ここへ分離した（#420）。移行手順そのものは本体側にある。

use authz::migrate::FromNs;

/// blob.object_key を移行先名前空間へ写す。
/// legacy: `{org}/...` → `{to}/{org}/...`。「移行済み」判定は `{to}/{org}/` **完全一致**で行う
/// （`{to}/` だけだと org == to の legacy キー `{to}/{sha}` を誤って移行済み扱いする）。
/// rename: `{from}/...` → `{to}/...`。移行対象でなければ `None`。
pub(crate) fn renamespace_object_key(
    old_key: &str,
    org: &str,
    from: &FromNs,
    to: &str,
) -> Option<String> {
    match from {
        FromNs::Legacy => {
            let migrated_prefix = format!("{to}/{org}/");
            (!old_key.starts_with(&migrated_prefix)).then(|| format!("{to}/{old_key}"))
        }
        FromNs::Tenant(f) => old_key
            .strip_prefix(&format!("{f}/"))
            .map(|rest| format!("{to}/{rest}")),
    }
}

/// [`renamespace_object_key`] の逆: **commit 済みの新キー**から移行元の旧キーを導出する
/// （手順4 の旧キー掃除用・#91 M-5）。
/// legacy: `{to}/{org}/{sha}` → `{org}/{sha}`（`{to}/` を剥がす）。
/// rename: `{to}/{rest}` → `{from}/{rest}`。
/// 新形式（`{to}/` 始まり）でないキーは `None`（触らない）。移行を経ていない行から
/// 導出された旧キーはオブジェクトストアに存在しないため、exists 確認後の削除は安全。
pub(crate) fn pre_migration_object_key(new_key: &str, from: &FromNs, to: &str) -> Option<String> {
    let rest = new_key.strip_prefix(&format!("{to}/"))?;
    match from {
        FromNs::Legacy => Some(rest.to_string()),
        FromNs::Tenant(f) => Some(format!("{f}/{rest}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_key_renamespace() {
        // legacy: org 直下キーへ tenant を前置。「移行済み」は {to}/{org}/ 完全一致で判定。
        let legacy = FromNs::Legacy;
        assert_eq!(
            renamespace_object_key("acme/deadbeef", "acme", &legacy, "t1").as_deref(),
            Some("t1/acme/deadbeef")
        );
        assert_eq!(
            renamespace_object_key("t1/acme/deadbeef", "acme", &legacy, "t1"),
            None
        );
        // org == to（legacy キー "acme/sha" を tenant acme へ移行）でも誤スキップしない。
        assert_eq!(
            renamespace_object_key("acme/deadbeef", "acme", &legacy, "acme").as_deref(),
            Some("acme/acme/deadbeef")
        );
        assert_eq!(
            renamespace_object_key("acme/acme/deadbeef", "acme", &legacy, "acme"),
            None
        );
        // rename: prefix 差し替え。他テナントは対象外。
        let rename = FromNs::Tenant("default".into());
        assert_eq!(
            renamespace_object_key("default/acme/deadbeef", "acme", &rename, "t1").as_deref(),
            Some("t1/acme/deadbeef")
        );
        assert_eq!(
            renamespace_object_key("other/acme/x", "acme", &rename, "t1"),
            None
        );
    }

    #[test]
    fn pre_migration_key_inverts_renamespace() {
        // 手順4（commit 後の旧キー掃除）: renamespace の往復が成立すること（#91 M-5）。
        let legacy = FromNs::Legacy;
        assert_eq!(
            pre_migration_object_key("t1/acme/deadbeef", &legacy, "t1").as_deref(),
            Some("acme/deadbeef")
        );
        let rename = FromNs::Tenant("default".into());
        assert_eq!(
            pre_migration_object_key("t1/acme/deadbeef", &rename, "t1").as_deref(),
            Some("default/acme/deadbeef")
        );
        // 新形式（{to}/ 始まり）でないキーは触らない。
        assert_eq!(
            pre_migration_object_key("other/acme/x", &rename, "t1"),
            None
        );
        // 往復: renamespace → pre_migration で元に戻る。
        let new_key =
            renamespace_object_key("default/acme/deadbeef", "acme", &rename, "t1").unwrap();
        assert_eq!(
            pre_migration_object_key(&new_key, &rename, "t1").as_deref(),
            Some("default/acme/deadbeef")
        );
    }
}
