//! GitHub 専用の認証・宛先束縛。鍵・token・応答本文をエラーに含めない。
mod transport;
use crate::{
    sink::{BugReportSink, ConfirmedBugReport, DeliveryIdentity, ReconcileOutcome, SendOutcome},
    Error, GitHubConfig,
};
use async_trait::async_trait;
use authz::AuthContext;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::time::Duration;
use tokio::sync::Mutex;

pub struct GitHubIssueSink {
    config: GitHubConfig,
    token: Mutex<Option<Token>>,
    #[cfg(test)]
    stub: Option<String>,
}
struct Token {
    value: String,
    expires: DateTime<Utc>,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Dispatch {
    Sent { id: String, url: String },
    Retry { after: u64, code: &'static str },
    Failed(&'static str),
    Unknown,
}
#[derive(Deserialize)]
struct Issue {
    number: u64,
    html_url: String,
    body: Option<String>,
    pull_request: Option<serde_json::Value>,
}
impl GitHubIssueSink {
    pub fn new(config: GitHubConfig) -> Result<Self, Error> {
        config.validate()?;
        // 起動時に検証。秘密鍵は token 再取得時にも読み直して rotation に追従する。
        read_key(&config)?;
        Ok(Self {
            config,
            token: Mutex::new(None),
            #[cfg(test)]
            stub: None,
        })
    }
    pub(crate) fn installation_id(&self) -> String {
        self.config.installation_id.to_string()
    }
    pub fn config_version(&self) -> i64 {
        self.config.config_version
    }
    async fn token(&self) -> Result<String, Dispatch> {
        let mut cached = self.token.lock().await;
        if let Some(token) = cached
            .as_ref()
            .filter(|t| t.expires.timestamp() > Utc::now().timestamp() + 120)
        {
            return Ok(token.value.clone());
        }
        let key = read_key(&self.config).map_err(|_| Dispatch::Failed("credential"))?;
        let now = Utc::now().timestamp();
        let jwt = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
            &serde_json::json!({"iat":now-60,"exp":now+540,"iss":self.config.app_id.to_string()}),
            &key,
        )
        .map_err(|_| Dispatch::Failed("credential"))?;
        let path = format!(
            "/app/installations/{}/access_tokens",
            self.config.installation_id
        );
        let (status, headers, bytes) = self.request(reqwest::Method::POST, &path, &jwt,
            Some(serde_json::json!({"repositories":[self.config.repo],"permissions":{"issues":"write","metadata":"read"}}))).await?;
        if status != 201 {
            // Issue POST 前の一時障害は配送の回数・期限上限内で再試行する。
            return Err(match classify(status, &headers, &bytes) {
                retry @ Dispatch::Retry { .. } => retry,
                Dispatch::Unknown => Dispatch::Retry {
                    after: 60,
                    code: "credential_unavailable",
                },
                _ => Dispatch::Failed("credential"),
            });
        }
        #[derive(Deserialize)]
        struct Response {
            token: String,
            expires_at: DateTime<Utc>,
        }
        let result: Response =
            serde_json::from_slice(&bytes).map_err(|_| Dispatch::Failed("credential"))?;
        if result.token.is_empty() || result.expires_at.timestamp() <= now + 120 {
            return Err(Dispatch::Failed("credential"));
        }
        let value = result.token.clone();
        *cached = Some(Token {
            value: result.token,
            expires: result.expires_at,
        });
        Ok(value)
    }
    fn issue_path(&self) -> String {
        format!("/repos/{}/{}/issues", self.config.owner, self.config.repo)
    }
    fn issue(&self, bytes: &[u8]) -> Option<Dispatch> {
        self.valid_issue(&serde_json::from_slice(bytes).ok()?)
    }
    fn valid_issue(&self, issue: &Issue) -> Option<Dispatch> {
        let expected = format!(
            "https://github.com/{}/{}/issues/{}",
            self.config.owner, self.config.repo, issue.number
        );
        (issue.number > 0 && issue.html_url == expected && issue.pull_request.is_none()).then(
            || Dispatch::Sent {
                id: issue.number.to_string(),
                url: expected,
            },
        )
    }
    pub(crate) async fn dispatch(
        &self,
        report: &ConfirmedBugReport,
        identity: &DeliveryIdentity,
    ) -> Dispatch {
        if report.content.validate().is_err() {
            return Dispatch::Failed("payload");
        }
        let token = match self.token().await {
            Ok(t) => t,
            // Issue 作成の POST 前なので結果不明にはしない（照合で見つからず永久に未配送になる）。
            Err(Dispatch::Unknown) => {
                return Dispatch::Retry {
                    after: 60,
                    code: "credential_unavailable",
                }
            }
            Err(e) => return e,
        };
        let c = &report.content;
        let body = format!(
            "## 種別\n{}\n\n## 症状\n{}\n\n## 試した手順\n{}\n\n## 期待動作\n{}\n\n## 環境情報\n未収集\n\n{}",
            category_label(c.category),
            literal(&c.symptom),
            literal(&c.steps_tried),
            literal(&c.expected_behavior),
            marker(identity)
        );
        match self
            .request(
                reqwest::Method::POST,
                &self.issue_path(),
                &token,
                Some(serde_json::json!({"title":neutralize(&c.title),"body":body})),
            )
            .await
        {
            Ok((201, _, bytes)) => self.issue(&bytes).unwrap_or(Dispatch::Unknown),
            Ok((status, headers, bytes)) => classify(status, &headers, &bytes),
            Err(e) => e,
        }
    }
    pub(crate) async fn lookup(&self, identity: &DeliveryIdentity) -> Dispatch {
        // 最大 3 ページ・30 秒。Link ヘッダの URL を追わない。
        match tokio::time::timeout(Duration::from_secs(30), self.lookup_pages(identity)).await {
            Ok(result) => result,
            Err(_) => Dispatch::Unknown,
        }
    }
    async fn lookup_pages(&self, identity: &DeliveryIdentity) -> Dispatch {
        let token = match self.token().await {
            Ok(token) => token,
            Err(
                retry @ Dispatch::Retry {
                    code: "rate_limit", ..
                },
            ) => return retry,
            Err(_) => return Dispatch::Unknown,
        };
        for page in 1..=3 {
            let path = format!(
                "{}?state=all&sort=created&direction=desc&per_page=100&page={page}",
                self.issue_path()
            );
            let bytes = match self
                .request(reqwest::Method::GET, &path, &token, None)
                .await
            {
                Ok((200, _, bytes)) => bytes,
                Ok((status, headers, bytes)) => {
                    return match classify(status, &headers, &bytes) {
                        retry @ Dispatch::Retry {
                            code: "rate_limit", ..
                        } => retry,
                        _ => Dispatch::Unknown,
                    }
                }
                Err(_) => return Dispatch::Unknown,
            };
            let Ok(issues) = serde_json::from_slice::<Vec<Issue>>(&bytes) else {
                return Dispatch::Unknown;
            };
            let count = issues.len();
            for issue in issues {
                if issue
                    .body
                    .as_deref()
                    .is_some_and(|b| b.lines().any(|line| line == marker(identity)))
                {
                    return self.valid_issue(&issue).unwrap_or(Dispatch::Unknown);
                }
            }
            if count < 100 {
                break;
            }
        }
        Dispatch::Unknown
    }
}
fn read_key(config: &GitHubConfig) -> Result<jsonwebtoken::EncodingKey, Error> {
    // 巨大ファイルは読まない。設定・鍵素材は Debug/Serialize を実装した型へ保持しない。
    let file = std::fs::File::open(&config.private_key_file).map_err(|_| Error::Invalid)?;
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Invalid)?;
    if bytes.len() > 64 * 1024 {
        return Err(Error::Invalid);
    }
    jsonwebtoken::EncodingKey::from_rsa_pem(&bytes).map_err(|_| Error::Invalid)
}
fn category_label(category: crate::Category) -> &'static str {
    // 本文を共有しない報告でも、許可された唯一の情報である種別は配送する。
    match category {
        crate::Category::Bug => "不具合",
        crate::Category::Usage => "使い方",
        crate::Category::Other => "その他",
    }
}
fn marker(identity: &DeliveryIdentity) -> String {
    format!("<!-- shiki-delivery:{} -->", identity.id)
}
fn neutralize(text: &str) -> String {
    text.chars()
        .flat_map(|c| match c {
            '@' => vec!['＠'],
            '.' => vec!['．'],
            '#' => vec!['＃'],
            '<' => vec!['＜'],
            '>' => vec!['＞'],
            '[' => vec!['［'],
            ']' => vec!['］'],
            '!' => vec!['！'],
            '`' => vec!['｀'],
            '\n' | '\r' => vec![' '],
            _ => vec![c],
        })
        .collect()
}
fn literal(text: &str) -> String {
    text.lines()
        .map(|line| format!("    {}", neutralize(line)))
        .collect::<Vec<_>>()
        .join("\n")
}
fn classify(status: u16, headers: &reqwest::header::HeaderMap, bytes: &[u8]) -> Dispatch {
    let retry = headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.parse::<u64>().ok().or_else(|| {
                DateTime::parse_from_rfc2822(v).ok().and_then(|t| {
                    u64::try_from((t.with_timezone(&Utc) - Utc::now()).num_seconds().max(0)).ok()
                })
            })
        });
    let reset = headers
        .get("x-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .and_then(|t| u64::try_from((t - Utc::now().timestamp()).max(0)).ok());
    // 本文は分類にだけ使用し、ログ・監査・エラーには渡さない。
    let secondary = status == 403
        && serde_json::from_slice::<serde_json::Value>(bytes)
            .ok()
            .and_then(|v| v.get("message")?.as_str().map(str::to_ascii_lowercase))
            .is_some_and(|message| message.contains("secondary rate limit"));
    let exhausted = headers
        .get("x-ratelimit-remaining")
        .is_some_and(|v| v == "0");
    if status == 429 || (status == 403 && (retry.is_some() || exhausted || secondary)) {
        return Dispatch::Retry {
            after: retry
                .unwrap_or(if exhausted || secondary { 0 } else { 60 })
                .max(if exhausted { reset.unwrap_or(60) } else { 0 })
                .max(if secondary { 60 } else { 0 })
                .max(1),
            code: "rate_limit",
        };
    }
    match status {
        401 => Dispatch::Failed("unauthorized"),
        403 => Dispatch::Failed("forbidden"),
        404 => Dispatch::Failed("not_found"),
        422 => Dispatch::Failed("validation"),
        300..=399 => Dispatch::Failed("redirect"),
        _ => Dispatch::Unknown,
    }
}
#[async_trait]
impl BugReportSink for GitHubIssueSink {
    async fn send(
        &self,
        _: &AuthContext,
        report: &ConfirmedBugReport,
        identity: &DeliveryIdentity,
    ) -> SendOutcome {
        match self.dispatch(report, identity).await {
            Dispatch::Sent { id, .. } => SendOutcome::Sent { external_id: id },
            Dispatch::Failed(_) | Dispatch::Retry { .. } => SendOutcome::Failed,
            Dispatch::Unknown => SendOutcome::Unknown,
        }
    }
    async fn reconcile(&self, _: &AuthContext, identity: &DeliveryIdentity) -> ReconcileOutcome {
        match self.lookup(identity).await {
            Dispatch::Sent { id, .. } => ReconcileOutcome::Found { external_id: id },
            _ => ReconcileOutcome::Inconclusive,
        }
    }
}
#[cfg(test)]
pub(crate) mod tests;
