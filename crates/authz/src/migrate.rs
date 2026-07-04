//! テナント名前空間の移行（`shiki-admin retenant` / #89）。
//!
//! - **LEGACY モード**（SAAS.1 の P1 フォロー）: 名前空間化以前の旧識別子
//!   （`folder:<uuid>` 等・tenant プレフィクス無し）を `<type>:<to>|<local>` へ移す。
//! - **リネームモード**（SAAS.5 cell→pool）: `<type>:<from>|<local>` を `<type>:<to>|<local>` へ移す。
//!
//! raw 識別子文字列の組み立て/解釈は本モジュール（authz crate 内）に閉じ、呼び出し側
//! （CLI）には露出しない。タプルの移行は「旧 object の全直接タプルを読み、subject/object の
//! 両方を新名前空間へ写して書き込み、旧タプルを剥奪」で行う（冪等・再実行で収束）。

use crate::{client::OpenFgaClient, error::AuthzError, object::TENANT_SEP, vocab::ObjectType};

/// 移行元の名前空間指定。
#[derive(Debug, Clone)]
pub enum FromNs {
    /// 名前空間化以前の旧識別子（tenant プレフィクス無し）。
    Legacy,
    /// 既存テナント名前空間（cell→pool リネーム元）。
    Tenant(String),
}

impl FromNs {
    /// local id へ移行元プレフィクスを付けた識別子 id 部を作る。
    fn qualify(&self, local: &str) -> String {
        match self {
            FromNs::Legacy => local.to_string(),
            FromNs::Tenant(t) => format!("{t}{TENANT_SEP}{local}"),
        }
    }

    /// 識別子 id 部（`type:` の後ろ）から local を取り出す。移行元に属さなければ `None`。
    fn strip<'a>(&self, id_part: &'a str) -> Option<&'a str> {
        match self {
            // legacy = プレフィクス無し。`|` を含む id は名前空間化済みなので対象外。
            FromNs::Legacy => (!id_part.contains(TENANT_SEP)).then_some(id_part),
            FromNs::Tenant(t) => id_part.strip_prefix(&format!("{t}{TENANT_SEP}")),
        }
    }
}

/// 識別子文字列（`type:id` または `type:id#relation`）を移行先名前空間へ写す。
/// 移行元名前空間に属さない識別子（他テナント等）は `None`（触らない）。
fn renamespace(raw: &str, from: &FromNs, to: &str) -> Option<String> {
    let (type_part, rest) = raw.split_once(':')?;
    // userset（`...#relation`）は relation を保存して id 部だけ写す。
    let (id_part, relation) = match rest.split_once('#') {
        Some((id, rel)) => (id, Some(rel)),
        None => (rest, None),
    };
    let local = from.strip(id_part)?;
    let new_id = format!("{to}{TENANT_SEP}{local}");
    Some(match relation {
        Some(rel) => format!("{type_part}:{new_id}#{rel}"),
        None => format!("{type_part}:{new_id}"),
    })
}

/// 1 オブジェクトの全直接タプルを移行先名前空間へ移す。
///
/// 返り値は `(移行したタプル数, スキップしたタプル数)`。スキップ＝subject が移行元
/// 名前空間に属さないもの（想定外の混入。呼び出し側でログする）。
/// `execute=false`（dry-run）では読み取りのみで件数を返す。
pub async fn retenant_object_tuples(
    client: &OpenFgaClient,
    object_type: ObjectType,
    local_id: &str,
    from: &FromNs,
    to: &str,
    execute: bool,
) -> Result<(u32, u32), AuthzError> {
    let old_object = format!("{}:{}", object_type.as_str(), from.qualify(local_id));
    let new_object = format!("{}:{}{}{}", object_type.as_str(), to, TENANT_SEP, local_id);
    let tuples = client
        .fga()
        .read_tuples(client.store_id(), &old_object, None)
        .await?;
    let mut moved: u32 = 0;
    let mut skipped: u32 = 0;
    for t in &tuples {
        let Some(new_user) = renamespace(&t.user, from, to) else {
            skipped += 1;
            continue;
        };
        if execute {
            client
                .fga()
                .write_tuple(
                    client.store_id(),
                    client.model_id(),
                    &new_user,
                    &t.relation,
                    &new_object,
                )
                .await?;
            client
                .fga()
                .delete_tuple(
                    client.store_id(),
                    client.model_id(),
                    &t.user,
                    &t.relation,
                    &old_object,
                )
                .await?;
        }
        moved += 1;
    }
    Ok((moved, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renamespace_legacy_identifiers() {
        let from = FromNs::Legacy;
        assert_eq!(
            renamespace("user:alice", &from, "acme").as_deref(),
            Some("user:acme|alice")
        );
        assert_eq!(
            renamespace("folder:f1", &from, "acme").as_deref(),
            Some("folder:acme|f1")
        );
        // userset は relation を保存。
        assert_eq!(
            renamespace("role:sales#member", &from, "acme").as_deref(),
            Some("role:acme|sales#member")
        );
        // 既に名前空間化済みの識別子は legacy 対象外（触らない）。
        assert_eq!(renamespace("user:other|bob", &from, "acme"), None);
    }

    #[test]
    fn renamespace_tenant_rename() {
        let from = FromNs::Tenant("default".into());
        assert_eq!(
            renamespace("user:default|alice", &from, "acme").as_deref(),
            Some("user:acme|alice")
        );
        assert_eq!(
            renamespace("role:default|sales#member", &from, "acme").as_deref(),
            Some("role:acme|sales#member")
        );
        // 他テナント・legacy は対象外。
        assert_eq!(renamespace("user:other|bob", &from, "acme"), None);
        assert_eq!(renamespace("user:alice", &from, "acme"), None);
    }

    #[test]
    fn from_ns_qualify_and_strip() {
        let legacy = FromNs::Legacy;
        assert_eq!(legacy.qualify("f1"), "f1");
        assert_eq!(legacy.strip("f1"), Some("f1"));
        assert_eq!(legacy.strip("t|f1"), None);
        let t = FromNs::Tenant("t1".into());
        assert_eq!(t.qualify("f1"), "t1|f1");
        assert_eq!(t.strip("t1|f1"), Some("f1"));
        assert_eq!(t.strip("t2|f1"), None);
    }
}
