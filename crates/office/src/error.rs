//! office クレートのエラー型。
//!
//! WOPI ハンドラ（`wopi::routes`）が HTTP ステータスへ写像する
//! （401=トークン不正 / 404=読めない・存在秘匿 / 409=ロック競合）。

/// office（OfficeSuite / WOPI）のエラー。
#[derive(Debug, thiserror::Error)]
pub enum OfficeError {
    /// access_token の検証失敗（署名不正・期限切れ・クレーム欠落・file_id 不一致）。
    /// 理由の内訳はクライアントへ返さない（fail-closed・オラクル防止）。
    #[error("認証に失敗しました")]
    Unauthorized,
    /// 対象が存在しない・読めない（存在秘匿の 404）。
    #[error("対象が見つかりません")]
    NotFound,
    /// 読めるが要求操作の権限が無い（viewer による書込等）。
    #[error("権限がありません")]
    Forbidden,
    /// WOPI ロック競合（409）。現ロック ID を X-WOPI-Lock ヘッダで返す。
    #[error("ロックが競合しています")]
    LockConflict {
        /// 現在有効なロック ID（無ロック起因の競合では空文字を返す＝WOPI 準拠）。
        current_lock_id: String,
    },
    /// Collabora discovery の取得・パース失敗（機能 off の fail-closed）。
    #[error("discovery の取得に失敗しました: {0}")]
    Discovery(String),
    /// リクエスト不正（X-WOPI-Override 不明値等）。
    #[error("不正なリクエスト: {0}")]
    Invalid(String),
    #[error(transparent)]
    Storage(#[from] storage::StorageError),
    #[error(transparent)]
    Authz(#[from] authz::AuthzError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}
