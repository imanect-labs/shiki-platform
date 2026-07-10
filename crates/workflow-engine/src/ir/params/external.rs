//! 外部連携（http.request / script.run / workflow.start）の params 契約（ir.md §7.2/§7.6）。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::ParamsIssue;
use crate::ir::expr::ValueExpr;

/// HTTP メソッド（閉集合・UI セレクト用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum HttpMethod {
    #[default]
    GET,
    POST,
    PUT,
    PATCH,
    DELETE,
    HEAD,
}

impl HttpMethod {
    /// リクエスト組み立てに使う文字列表現。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            HttpMethod::GET => "GET",
            HttpMethod::POST => "POST",
            HttpMethod::PUT => "PUT",
            HttpMethod::PATCH => "PATCH",
            HttpMethod::DELETE => "DELETE",
            HttpMethod::HEAD => "HEAD",
        }
    }
}

/// secret の添付方式（既定 bearer）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SecretAttachKind {
    /// `Authorization: Bearer <value>`（既定）。
    #[default]
    Bearer,
    /// 指定ヘッダに値をそのまま入れる。
    Header,
}

/// secret の添付指定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SecretAttach {
    /// 添付方式。
    pub kind: SecretAttachKind,
    /// kind=header 時のヘッダ名（省略時 Authorization）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub header: Option<String>,
}

/// secret 参照（参照名のみ・解決/宛先束縛検証/注入はエンジンが実行直前に行う）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct HttpSecretRef {
    /// secret の参照名（V4 で存在＋宛先束縛を照合）。
    pub name: String,
    /// 添付方式（省略時 bearer）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub attach: Option<SecretAttach>,
}

/// リダイレクトの扱い。Stage A は非追従のみ（`follow_stripped` は追従先の宛先束縛
/// 再照合の実装時に variant 追加する・PIT-36・偽装しない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RedirectPolicy {
    /// 3xx を拒否する（既定・唯一のサポート値）。
    #[default]
    Deny,
}

/// `http.request` — egress allowlist × シークレット宛先束縛の AND（best-effort・ir.md §7.2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct HttpRequestParams {
    /// HTTP メソッド（省略時 GET）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub method: Option<HttpMethod>,
    /// ベース URL。**ホスト部はリテラル必須**（`$from`/`$template` 不可・String 型が
    /// 構造的に拒否する＝PIT-36 の型防御。パスは `path_suffix` で可変にできる）。
    pub url: String,
    /// URL 末尾に連結するパス（可変部）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path_suffix: Option<ValueExpr>,
    /// リクエストボディ。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub body: Option<ValueExpr>,
    /// 添付する secret（参照名のみ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub secret: Option<HttpSecretRef>,
    /// リダイレクトの扱い（既定 deny・Stage A は deny のみ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub redirect: Option<RedirectPolicy>,
}

impl HttpRequestParams {
    /// URL の静的整合（scheme・解析可能性）を検査する。宛先束縛照合は V4/実行時。
    pub fn check_cross_fields(&self) -> Result<(), ParamsIssue> {
        let issue = |message: String| ParamsIssue {
            path: "/params/url".to_string(),
            message,
        };
        let parsed =
            url::Url::parse(&self.url).map_err(|e| issue(format!("url が解析できません: {e}")))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(issue("url は http/https のみ許可".to_string()));
        }
        if parsed.host_str().is_none() {
            return Err(issue("url にホストがありません".to_string()));
        }
        Ok(())
    }
}

/// script の source（inline / artifact のちょうど一方）。
///
/// untagged enum は serde の `deny_unknown_fields` と併用できないため、フラット struct
/// ＋ [`check_exactly_one`](Self::check_exactly_one) で厳密性を保つ。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ScriptSourceSpec {
    /// インラインソース（≤64KB・保存時に swc パース検証 V6）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub inline: Option<String>,
    /// artifact 参照（`script:<name>@<ver>`・version 固定）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub artifact: Option<String>,
}

impl ScriptSourceSpec {
    /// inline / artifact のちょうど一方が指定されていることを検査する。
    pub fn check_exactly_one(&self) -> Result<(), ParamsIssue> {
        match (self.inline.is_some(), self.artifact.is_some()) {
            (true, false) | (false, true) => Ok(()),
            _ => Err(ParamsIssue {
                path: "/params/source".to_string(),
                message: "source は inline / artifact のどちらか一方を指定してください".to_string(),
            }),
        }
    }
}

/// `script.run` — shiki script の 1 回有界実行（ir.md §7.6）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ScriptRunParams {
    /// スクリプトソース。
    pub source: ScriptSourceSpec,
    /// `main(input)` へ渡す入力（省略時はステップ入力）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub input: Option<ValueExpr>,
}

/// `workflow.start` — 別ワークフロー起動（fire-and-forget・起動 1 回保証）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct WorkflowStartParams {
    /// 起動するワークフロー name。
    pub name: ValueExpr,
    /// 子 run への入力。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub input: Option<ValueExpr>,
}
