use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BugReportConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub backend: BugReportBackend,
    #[serde(default)]
    pub github: Option<GitHubConfig>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BugReportBackend {
    #[default]
    Disabled,
    Github,
}
/// 秘密鍵そのものは設定に持たず、mount したファイルを参照する。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubConfig {
    pub app_id: u64,
    pub installation_id: u64,
    pub private_key_file: std::path::PathBuf,
    pub owner: String,
    pub repo: String,
    pub profile: String,
    /// 宛先変更時に必ず増やす。提出済み snapshot を別の設定へ配送しない。
    pub config_version: i64,
}
impl GitHubConfig {
    pub fn validate(&self) -> Result<(), crate::Error> {
        if self.profile != "preview"
            || self.owner != "imanect-labs"
            || self.repo != "shiki-test"
            || self.app_id == 0
            || self.installation_id == 0
            || self.config_version < 1
            || !self.private_key_file.is_absolute()
        {
            return Err(crate::Error::Invalid);
        }
        Ok(())
    }
}
impl BugReportConfig {
    pub fn validate(&self) -> Result<(), crate::Error> {
        if self.backend == BugReportBackend::Github {
            self.github
                .as_ref()
                .ok_or(crate::Error::Invalid)?
                .validate()?;
        }
        Ok(())
    }
}
