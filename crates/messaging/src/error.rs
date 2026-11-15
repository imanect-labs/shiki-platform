/// messaging のエラー。
///
/// 読めないチャンネル・発言は [`Error::NotFound`] で返し、存在を開示しない
/// （非公開チャンネルと他人の DM は非メンバーには存在ごと見えない）。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 対象が無い、または読めない（存在を開示しない）。
    #[error("not found")]
    NotFound,
    /// 読めるが、その操作は許されない（投稿・管理・退出の制約など）。
    #[error("forbidden: {0}")]
    Forbidden(&'static str),
    /// 入力が不正。
    #[error("invalid: {0}")]
    Invalid(String),
    /// 認可基盤の失敗（fail-closed で拒否側に倒す）。
    #[error("authz: {0}")]
    Authz(#[from] authz::AuthzError),
    /// DB の失敗。
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
}
