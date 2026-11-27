//! span へのエラー記録（`#[instrument(err)]` の代わり）。
//!
//! `err` はエラーの Display をそのまま span に載せる。ObjectStore のキーや OpenFGA の応答本文、
//! DB のエラー文言などの生の文字列をトレース・ログへ出さないよう、**エラーの種別だけ**を ERROR
//! イベントとして記録する（ERROR イベントで span は error 状態になる）。

use crate::{error::StorageError, object_store::ObjectStoreError};

fn object_store_kind(e: &ObjectStoreError) -> &'static str {
    match e {
        ObjectStoreError::NotFound(_) => "not_found",
        ObjectStoreError::Backend(_) => "backend",
        ObjectStoreError::Presign(_) => "presign",
    }
}

fn storage_kind(e: &StorageError) -> &'static str {
    match e {
        StorageError::Forbidden => "forbidden",
        StorageError::NotFound => "not_found",
        StorageError::Conflict => "conflict",
        StorageError::VersionConflict { .. } => "version_conflict",
        StorageError::Locked => "locked",
        StorageError::Invalid(_) => "invalid",
        StorageError::Integrity(_) => "integrity",
        StorageError::ObjectStore(_) => "object_store",
        StorageError::Db(_) => "db",
        StorageError::Authz(_) => "authz",
    }
}

/// StorageService の結果を span に記録する（失敗なら種別だけを ERROR で残す）。
pub(crate) fn storage<T>(r: Result<T, StorageError>) -> Result<T, StorageError> {
    if let Err(e) = &r {
        let cause = match e {
            StorageError::ObjectStore(inner) => Some(object_store_kind(inner)),
            _ => None,
        };
        tracing::error!(
            error.kind = storage_kind(e),
            error.cause = cause,
            "ストレージ操作に失敗"
        );
    }
    r
}

/// ObjectStore の結果を span に記録する（キー・バックエンドの文言は載せない）。
pub(crate) fn object_store<T>(r: Result<T, ObjectStoreError>) -> Result<T, ObjectStoreError> {
    if let Err(e) = &r {
        tracing::error!(
            error.kind = object_store_kind(e),
            "オブジェクトストア操作に失敗"
        );
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_do_not_carry_raw_text() {
        let e = ObjectStoreError::NotFound("tenant/org/secret-key".into());
        assert_eq!(object_store_kind(&e), "not_found");
        assert_eq!(
            storage_kind(&StorageError::ObjectStore(ObjectStoreError::Backend(
                "body".into()
            ))),
            "object_store"
        );
        for (e, k) in [
            (StorageError::Forbidden, "forbidden"),
            (StorageError::NotFound, "not_found"),
            (StorageError::Conflict, "conflict"),
            (
                StorageError::VersionConflict { current: 2 },
                "version_conflict",
            ),
            (StorageError::Locked, "locked"),
            (StorageError::Invalid("x".into()), "invalid"),
            (StorageError::Integrity("x".into()), "integrity"),
            (StorageError::Db(sqlx::Error::RowNotFound), "db"),
        ] {
            assert_eq!(storage_kind(&e), k);
        }
        assert_eq!(
            object_store_kind(&ObjectStoreError::Presign("x".into())),
            "presign"
        );
        // 失敗・成功のどちらも値はそのまま返す。
        assert!(storage::<()>(Err(StorageError::Forbidden)).is_err());
        assert!(object_store::<()>(Err(e)).is_err());
        assert_eq!(storage(Ok::<_, StorageError>(1)).unwrap(), 1);
    }
}
