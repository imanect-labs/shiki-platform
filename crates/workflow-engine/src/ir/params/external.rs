//! 外部連携（http.request / script.run / workflow.start）の params 契約（ir.md §7.2/§7.6）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::ParamsIssue;
use crate::ir::expr::ValueExpr;
use crate::nodes::http_headers::{
    check_headers, check_secret_header_on, HeaderSurface, HeaderValue,
};

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
    /// 作成者が決めるヘッダー（名前 → 値・省略可・GAP-19）。名前はリテラルで、値は差し込み・参照を書ける。
    ///
    /// 送れるヘッダーの決まりと上限は script の `Shiki.http.request` と同じ（`nodes::http_headers`）: Shiki が決める
    /// ヘッダー（Host・Content-Length など）・シークレットを付けるヘッダーと同じ名前・形の誤り・制御文字・重複・上限
    /// （20 個・値 4 KB・合計 8 KB）は、保存時（V1）と実行時に断る。`Idempotency-Key`・`Content-Type` を書くと、Shiki が
    /// 付ける値の代わりにそれを送る。省略はこの項目が無かったころの定義の挙動（作成者のヘッダーを付けない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub headers: Option<BTreeMap<String, ValueExpr>>,
    /// リダイレクトの扱い（既定 deny・Stage A は deny のみ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub redirect: Option<RedirectPolicy>,
    /// 相手が誤り（400 以上）を返したらブロックを失敗にするか（BUG-99・D-95）。
    ///
    /// 省略は `false`（この項目が無かったころの定義の挙動: 誤りの応答も成功として次へ渡す）。エディタで新しく
    /// 置くブロックは `true` を入れる。失敗の code は 4xx が `http_error_status`（やり直さない）、5xx と 408・425・429
    /// が `http_retryable_status`（試す回数の規則でやり直す）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fail_on_error_status: Option<bool>,
}

impl HttpRequestParams {
    /// URL の静的整合（scheme・解析可能性）と、ヘッダーの決まり（シークレットを付けるヘッダーの名前・作成者の
    /// ヘッダー・BUG-184・GAP-19）を検査する。宛先束縛照合は V4/実行時。
    pub fn check_cross_fields(&self) -> Result<(), ParamsIssue> {
        self.check_headers()?;
        let issue = |message: String| ParamsIssue {
            expected: Vec::new(),
            path: "/params/url".to_string(),
            message,
        };
        // 空の URL は保存を止めない（置いたばかりのブロックを保存できるように、実行と自動実行のオンの前に止める・
        // readiness・D-83）。
        if self.url.trim().is_empty() {
            return Ok(());
        }
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

    /// シークレットを付けるヘッダーの名前（ヘッダーに付けるとき）。シークレットを使わないなら `None`。
    #[must_use]
    pub fn secret_header(&self) -> Option<&str> {
        let secret = self.secret.as_ref()?;
        Some(match &secret.attach {
            Some(SecretAttach {
                kind: SecretAttachKind::Header,
                header: Some(name),
            }) => name.as_str(),
            _ => "Authorization",
        })
    }

    /// ヘッダーの決まりを、保存時に分かる範囲で当てる（値が差し込み・参照のものは実行時に確かめる）。
    fn check_headers(&self) -> Result<(), ParamsIssue> {
        let secret_header = self.secret_header();
        if let Some(name) = secret_header {
            check_secret_header_on(HeaderSurface::Block, name).map_err(|refused| ParamsIssue {
                expected: Vec::new(),
                path: "/params/secret/attach/header".to_string(),
                message: refused.into_message(),
            })?;
        }
        let Some(headers) = &self.headers else {
            return Ok(());
        };
        let entries: Vec<(&str, HeaderValue<'_>)> = headers
            .iter()
            .map(|(name, value)| {
                let value = match value {
                    ValueExpr::Literal(serde_json::Value::String(s)) => HeaderValue::Text(s),
                    ValueExpr::Literal(_) => HeaderValue::NotText,
                    ValueExpr::From(_) | ValueExpr::Template(_) => HeaderValue::Later,
                };
                (name.as_str(), value)
            })
            .collect();
        check_headers(HeaderSurface::Block, &entries, secret_header).map_err(|refused| {
            ParamsIssue {
                expected: Vec::new(),
                path: "/params/headers".to_string(),
                message: refused.into_message(),
            }
        })
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
                expected: Vec::new(),
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::{check_params, ParamsIssue};
    use crate::vocab::NodeType;

    fn check(params: &serde_json::Value) -> Result<(), ParamsIssue> {
        check_params(NodeType::HttpRequest, params)
    }

    fn issue(path: &str, message: &str) -> Result<(), ParamsIssue> {
        Err(ParamsIssue {
            expected: Vec::new(),
            path: path.to_string(),
            message: message.to_string(),
        })
    }

    /// 「ヘッダー」は省略でき（この欄が無かったころの定義のまま保存できる）、値は差し込み・参照を書ける（GAP-19）。
    #[test]
    fn headers_are_optional_and_values_may_be_expressions() {
        assert_eq!(check(&json!({ "url": "https://api.example.com" })), Ok(()));
        assert_eq!(
            check(&json!({
                "url": "https://api.example.com",
                "headers": {
                    "Accept": "application/json",
                    "X-Request-Id": { "$from": "input", "path": "/id" },
                    "X-Title": { "$template": "件名 {t}", "vars": { "t": { "$from": "input", "path": "/t" } } }
                }
            })),
            Ok(())
        );
    }

    /// 送ってはいけないヘッダーは保存の時点で欄の下に言う（実行時と同じ決まり・GAP-19）。
    #[test]
    fn headers_that_cannot_be_sent_are_refused_on_save() {
        assert_eq!(
            check(
                &json!({ "url": "https://api.example.com", "headers": { "Host": "evil.example" } })
            ),
            issue(
                "/params/headers",
                "ヘッダー「Host」は Shiki が決めるため、指定できません。"
            )
        );
        assert_eq!(
            check(&json!({ "url": "https://api.example.com", "headers": { "X-Count": 3 } })),
            issue(
                "/params/headers",
                "ヘッダー「X-Count」の値は文字にしてください。"
            )
        );
        assert_eq!(
            check(&json!({
                "url": "https://api.example.com",
                "secret": { "name": "tok" },
                "headers": { "Authorization": "Basic eA==" }
            })),
            issue(
                "/params/headers",
                "ヘッダー「Authorization」は、シークレットを付けるヘッダーと同じため送れません。シークレットの付け方か、\
                 ヘッダーの名前を変えてください。"
            )
        );
        // シークレットを使わないなら Authorization も書ける。
        assert_eq!(
            check(
                &json!({ "url": "https://api.example.com", "headers": { "Authorization": "Basic eA==" } })
            ),
            Ok(())
        );
    }

    /// シークレットを付けるヘッダーの名前は、保存の時点で確かめる（Host などに平文を載せない・BUG-184）。
    #[test]
    fn the_header_the_secret_is_attached_to_is_checked_on_save() {
        let with = |header: &str| {
            json!({
                "url": "https://api.example.com",
                "secret": { "name": "tok", "attach": { "kind": "header", "header": header } }
            })
        };
        assert_eq!(check(&with("X-Api-Key")), Ok(()));
        assert_eq!(
            check(&with("Host")),
            issue(
                "/params/secret/attach/header",
                "シークレットを付けるヘッダー「Host」は Shiki が決めるため、指定できません。"
            )
        );
        for bad in ["", "X Api", "X-Api\r\nHost: evil"] {
            let e = check(&with(bad)).unwrap_err();
            assert_eq!(e.path, "/params/secret/attach/header", "{bad:?}");
            assert!(
                e.message
                    .starts_with("シークレットを付けるヘッダーの名前「"),
                "{bad:?}: {}",
                e.message
            );
        }
        // Bearer（既定）と、名前を省いたヘッダー（Authorization）は通る。
        assert_eq!(
            check(&json!({ "url": "https://api.example.com", "secret": { "name": "tok" } })),
            Ok(())
        );
        assert_eq!(
            check(&json!({
                "url": "https://api.example.com",
                "secret": { "name": "tok", "attach": { "kind": "header" } }
            })),
            Ok(())
        );
    }
}
