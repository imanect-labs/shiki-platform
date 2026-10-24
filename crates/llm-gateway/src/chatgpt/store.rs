//! ChatGPT プラン接続の資格情報ファイル（1 ファイル・JSON）。
//!
//! トークンを含むので、所有者だけが読める権限（Unix は `0600`）で書き、同じディレクトリの
//! 一時ファイルから rename で置き換える（書きかけのファイルを読ませない・OpenAI の推奨どおり）。
//! `Debug` は実装しない（トークンをログへ出さない）。

use std::io::Write;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{ChatGptModel, Owner};

/// ファイルの版（形を変えたら上げる）。
const VERSION: u32 = 1;

/// 資格情報ファイルの中身。
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Credentials {
    #[serde(default)]
    pub version: u32,
    /// このホストの識別子（`urn:uuid:…`）。初回ログインの前に決め、切断しても変えない。
    #[serde(default)]
    pub ext_agent_host_id: Option<String>,
    /// OpenAI が発行したクライアント ID（`oaiapp_…`）。再ログインで使い回す。
    #[serde(default)]
    pub client_id: Option<String>,
    /// 連携した本人（このプラン枠を使える唯一の主体）。
    #[serde(default)]
    pub owner: Option<Owner>,
    /// ChatGPT アカウント（表示と再ログインの照合用）。
    #[serde(default)]
    pub account: Option<AccountInfo>,
    /// 最後のサインインの ID トークン（再ログインの `id_token_hint`）。
    #[serde(default)]
    pub id_token: Option<String>,
    /// 使えるトークン。`None` は未接続、または再ログインが必要（リフレッシュの失効）。
    #[serde(default)]
    pub tokens: Option<Tokens>,
    #[serde(default)]
    pub connected_at: Option<DateTime<Utc>>,
    /// 接続時に取得した、このアカウントで使えるモデル（表示用）。
    #[serde(default)]
    pub models: Vec<ChatGptModel>,
}

/// ChatGPT アカウントの識別。
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct AccountInfo {
    /// ID トークンの `sub`（アカウントの照合はメールではなくこれで行う）。
    pub sub: String,
    #[serde(default)]
    pub email: Option<String>,
}

/// アクセストークンとリフレッシュトークン。
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    /// アクセストークンの失効時刻。
    pub expires_at: DateTime<Utc>,
    /// これより前にはリフレッシュしない（応答の `earliest_refresh_at`）。
    #[serde(default)]
    pub earliest_refresh_at: Option<DateTime<Utc>>,
}

impl Credentials {
    /// 接続を外す（ホスト ID だけ残す。次のログインは新規登録からやり直す）。
    pub(super) fn clear_connection(&mut self) {
        *self = Credentials {
            version: VERSION,
            ext_agent_host_id: self.ext_agent_host_id.take(),
            ..Credentials::default()
        };
    }
}

/// 読み込む。ファイルが無ければ `None`。
pub(super) fn load(path: &Path) -> std::io::Result<Option<Credentials>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// 書き込む（所有者のみ読める一時ファイル → rename）。
pub(super) fn save(path: &Path, creds: &Credentials) -> std::io::Result<()> {
    let mut creds = creds.clone();
    creds.version = VERSION;
    let bytes = serde_json::to_vec_pretty(&creds)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let res = (|| {
        let mut file = open_private(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

#[cfg(unix)]
fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path() -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!("shiki-chatgpt-{}", uuid::Uuid::new_v4()))
            .join("creds.json")
    }

    #[test]
    fn missing_file_loads_as_none() {
        assert!(load(&temp_path()).unwrap().is_none());
    }

    #[test]
    fn save_then_load_round_trips_with_owner_only_permissions() {
        let path = temp_path();
        let creds = Credentials {
            ext_agent_host_id: Some("urn:uuid:h".into()),
            client_id: Some("oaiapp_x".into()),
            tokens: Some(Tokens {
                access_token: "at".into(),
                refresh_token: "rt".into(),
                expires_at: Utc::now(),
                earliest_refresh_at: None,
            }),
            ..Credentials::default()
        };
        save(&path, &creds).unwrap();
        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(loaded.version, VERSION);
        assert_eq!(loaded.client_id.as_deref(), Some("oaiapp_x"));
        assert_eq!(loaded.tokens.unwrap().refresh_token, "rt");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // 一時ファイルを残さない。
        let dir = path.parent().unwrap();
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn broken_file_is_an_error_not_an_empty_connection() {
        let path = temp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{not json").unwrap();
        assert!(load(&path).is_err());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn clearing_keeps_only_the_host_id() {
        let mut creds = Credentials {
            ext_agent_host_id: Some("urn:uuid:h".into()),
            client_id: Some("oaiapp_x".into()),
            id_token: Some("id".into()),
            ..Credentials::default()
        };
        creds.clear_connection();
        assert_eq!(creds.ext_agent_host_id.as_deref(), Some("urn:uuid:h"));
        assert!(creds.client_id.is_none() && creds.id_token.is_none() && creds.owner.is_none());
    }
}
