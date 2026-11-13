use serde::{Deserialize, Serialize};
/// PR1 で選べるのは disabled のみ。github/stub は起動設定で拒否する。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BugReportConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub backend: BugReportBackend,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BugReportBackend {
    #[default]
    Disabled,
}
