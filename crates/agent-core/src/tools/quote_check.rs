//! `quote_check`: レポートの引用文字列が証拠台帳に実在するかを機械で確かめる（#409）。
//!
//! 検証を丸ごと機械化はしない。機械が持つのは**「引用した文字列が台帳のどこにも無い」**という、
//! 書式に依存せずに判定できる一点だけ（幻覚した引用＝最も危険な誤りを確実に拾う）。
//! 「その証拠がその主張を支持するか」「過剰な一般化」は意味の判断なので検証サブエージェントが持つ。
//!
//! 照合は列のパースをしない。台帳全体を正規化（空白・約物を落とし、全角英数を半角へ）した文字列に、
//! 引用を同じく正規化して部分一致させる。モデルが自由記述で書く台帳の書式の揺れ（列の区切り・
//! 改行・鉤括弧の付け方）で誤報しないため。省略記号（…）を挟んだ引用は、断片ごとに探す。

use std::sync::Arc;

use authz::AuthContext;

use crate::tool::{Tool, ToolError, ToolOutcome};
use crate::workspace::WorkspaceStore;

/// これより短い引用（正規化後の字数）は見ない（用語・ラベルの鉤括弧で誤報しないため）。
const MIN_QUOTE_CHARS: usize = 8;
/// 省略記号で割った断片のうち、これより短いものは照合しない（つなぎの語で誤報しないため）。
const MIN_FRAGMENT_CHARS: usize = 4;
/// 観測に並べる「見つからなかった引用」の上限（長いレポートでも観測を肥大させない）。
const MAX_LISTED: usize = 30;

pub struct QuoteCheckTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl QuoteCheckTool {
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        QuoteCheckTool { workspace }
    }
}

/// 照合結果。
#[derive(Debug, PartialEq, Eq)]
struct CheckReport {
    /// 照合した引用の数。
    checked: usize,
    /// 台帳に見つからなかった引用（レポートに書かれたままの文字列）。
    missing: Vec<String>,
}

/// 照合用の正規化: 空白・約物・括弧を落とし、全角英数記号を半角へ寄せ、英字を小文字にする。
fn normalize(text: &str) -> String {
    text.chars()
        .filter_map(|c| {
            // 全角 ASCII（！〜～）→ 半角。
            let c = match c as u32 {
                0xFF01..=0xFF5E => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
                0x3000 => ' ',
                _ => c,
            };
            if c.is_whitespace() || c.is_ascii_punctuation() || is_cjk_punct(c) {
                None
            } else {
                Some(c.to_ascii_lowercase())
            }
        })
        .collect()
}

/// 照合で無視する和文の約物・括弧（書き手ごとに揺れる）。
fn is_cjk_punct(c: char) -> bool {
    matches!(
        c,
        '、' | '。'
            | '・'
            | '…'
            | '‥'
            | '「'
            | '」'
            | '『'
            | '』'
            | '（'
            | '）'
            | '【'
            | '】'
            | '〔'
            | '〕'
            | '〈'
            | '〉'
            | '《'
            | '》'
            | '“'
            | '”'
            | '‘'
            | '’'
            | 'ー'
            | '―'
            | '—'
            | '－'
            | '〜'
            | '～'
            | '｜'
    )
}

/// レポートから引用（「」『』“” で囲まれた文字列）を出現順に取り出す。
fn extract_quotes(report: &str) -> Vec<String> {
    let mut quotes = Vec::new();
    let mut open: Option<(char, String)> = None;
    for c in report.chars() {
        match &mut open {
            None => {
                if let Some(close) = closing(c) {
                    open = Some((close, String::new()));
                }
            }
            Some((close, buf)) => {
                if c == *close {
                    quotes.push(std::mem::take(buf));
                    open = None;
                } else if c == '\n' {
                    // 引用は改行しない（台帳の書式）。閉じないまま改行したら引用ではない。
                    open = None;
                } else {
                    buf.push(c);
                }
            }
        }
    }
    quotes
}

/// 開き括弧 → 閉じ括弧。
fn closing(c: char) -> Option<char> {
    match c {
        '「' => Some('」'),
        '『' => Some('』'),
        '“' => Some('”'),
        _ => None,
    }
}

/// 引用の各断片（省略記号で割る）が、正規化した台帳に全部含まれるか。
fn found_in(quote: &str, ledger: &str) -> bool {
    quote
        .split(['…', '‥'])
        .flat_map(|s| s.split("..."))
        .map(normalize)
        .filter(|f| f.chars().count() >= MIN_FRAGMENT_CHARS)
        .all(|f| ledger.contains(&f))
}

/// レポートの引用を台帳と突き合わせる。
fn check(report: &str, ledger: &str) -> CheckReport {
    let ledger = normalize(ledger);
    let mut seen = std::collections::HashSet::new();
    let mut checked = 0;
    let mut missing = Vec::new();
    for quote in extract_quotes(report) {
        if normalize(&quote).chars().count() < MIN_QUOTE_CHARS || !seen.insert(quote.clone()) {
            continue;
        }
        checked += 1;
        if !found_in(&quote, &ledger) {
            missing.push(quote);
        }
    }
    CheckReport { checked, missing }
}

/// 照合結果 → モデルが読む観測。
fn observation(report_name: &str, ledger_name: &str, r: &CheckReport) -> String {
    use std::fmt::Write as _;
    if r.checked == 0 {
        return format!(
            "{report_name} に照合できる引用（{MIN_QUOTE_CHARS} 字以上の「」『』“”）はありませんでした。"
        );
    }
    if r.missing.is_empty() {
        return format!(
            "{report_name} の引用 {} 件はすべて {ledger_name} に見つかりました（文字列の実在のみ。\
             その証拠が主張を支持するかは別に確かめる）。",
            r.checked
        );
    }
    let mut s = format!(
        "{report_name} の引用 {} 件のうち {} 件が {ledger_name} に見つかりません（空白・約物の違いは\
         無視して照合済み）。台帳に無い引用は幻覚か転記ミスの疑いが強い。\n",
        r.checked,
        r.missing.len()
    );
    for q in r.missing.iter().take(MAX_LISTED) {
        let _ = writeln!(s, "- 「{}」", crate::cite::defuse(q));
    }
    if r.missing.len() > MAX_LISTED {
        let _ = writeln!(s, "（ほか {} 件）", r.missing.len() - MAX_LISTED);
    }
    s
}

#[async_trait::async_trait]
impl Tool for QuoteCheckTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        crate::vocab::ToolName::QuoteCheck.as_str()
    }

    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "レポート中の引用（「」『』“” で囲んだ 8 字以上の文字列）が、証拠台帳に実在するかを機械で\
         照合し、台帳に見つからない引用だけを返す。空白・約物の違いは無視し、省略記号（…）で\
         割った断片はそれぞれ探す。主張を証拠が支持するかは判定しない（それは読んで確かめる）。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "report": { "type": "string", "description": "レポートのファイル名（既定 report.md）" },
                "ledger": { "type": "string", "description": "証拠台帳のファイル名（既定 notes.md）" }
            },
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        false
    }

    // 読み取りのみ・冪等。
    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let name = |key: &str, default: &str| {
            input
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(default)
                .to_string()
        };
        let report_name = name("report", "report.md");
        let ledger_name = name("ledger", "notes.md");
        let report = self.workspace.read(ctx, &report_name, trace_id).await?;
        let ledger = self.workspace.read(ctx, &ledger_name, trace_id).await?;
        let r = check(
            &String::from_utf8_lossy(&report),
            &String::from_utf8_lossy(&ledger),
        );
        Ok(ToolOutcome::ok(observation(&report_name, &ledger_name, &r)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEDGER: &str = "\
E12 | https://example.go.jp/report | 2026-07-30 | 一次 | 国内市場は1.2兆円 | 「市場規模は1兆2000億円に達した」
E13 | 社内: 就業規則.docx | 2026-10-01 | 一次 | 育休の延長 | 保育所に入所できない場合は、子が1歳6か月に達するまで延長できる
";

    #[test]
    fn finds_quotes_regardless_of_spacing_and_punctuation() {
        let report = "市場は拡大した。「市場規模は 1兆2000億円に達した。」[E12]\n\
                      規程では『保育所に入所できない場合は子が１歳６か月に達するまで延長できる』とある。";
        assert_eq!(
            check(report, LEDGER),
            CheckReport {
                checked: 2,
                missing: vec![]
            }
        );
    }

    #[test]
    fn reports_quotes_missing_from_the_ledger() {
        let report =
            "「市場規模は2兆円を超えると予測された」[E12] と「市場規模は1兆2000億円に達した」";
        let r = check(report, LEDGER);
        assert_eq!(r.checked, 2);
        assert_eq!(
            r.missing,
            vec!["市場規模は2兆円を超えると予測された".to_string()]
        );
        let text = observation("report.md", "notes.md", &r);
        assert!(text.contains("2 件のうち 1 件が notes.md に見つかりません"));
        assert!(text.contains("- 「市場規模は2兆円を超えると予測された」"));
    }

    #[test]
    fn ellipsis_splits_quote_into_fragments() {
        // 省略記号の前後はそれぞれ台帳にある（間を省いた引用は正当）。
        let ok = "「保育所に入所できない場合は…1歳6か月に達するまで延長できる」";
        assert!(check(ok, LEDGER).missing.is_empty());
        // 片方の断片が台帳に無ければ見つからない扱い。
        let bad = "「保育所に入所できない場合は…3歳まで延長できる特例がある」";
        assert_eq!(check(bad, LEDGER).missing.len(), 1);
    }

    #[test]
    fn short_terms_and_unclosed_brackets_are_ignored() {
        // 用語の鉤括弧（8 字未満）と、閉じずに改行した括弧は引用として扱わない。
        let report = "「出典1系統」と書く。「閉じない括弧が\n次の行に続く文章で終わる」";
        let r = check(report, LEDGER);
        assert_eq!(r.checked, 0);
        assert!(observation("report.md", "notes.md", &r).contains("照合できる引用"));
    }

    #[test]
    fn duplicate_quotes_are_counted_once() {
        let report =
            "「市場規模は2兆円を超えると予測された」と「市場規模は2兆円を超えると予測された」";
        let r = check(report, LEDGER);
        assert_eq!((r.checked, r.missing.len()), (1, 1));
    }

    #[test]
    fn normalize_folds_width_case_and_punctuation() {
        assert_eq!(normalize("ＡＢＣ　１２３、「x-y」"), "abc123xy");
    }

    /// 読み取りだけを持つワークスペース（書込系は呼ばれない）。
    struct ReadOnly(std::collections::HashMap<String, String>);

    #[async_trait::async_trait]
    impl WorkspaceStore for ReadOnly {
        async fn list(
            &self,
            _ctx: &AuthContext,
            _trace_id: Option<&str>,
        ) -> Result<Vec<crate::workspace::WorkspaceEntry>, ToolError> {
            Ok(vec![])
        }
        async fn read(
            &self,
            _ctx: &AuthContext,
            name: &str,
            _trace_id: Option<&str>,
        ) -> Result<Vec<u8>, ToolError> {
            self.0
                .get(name)
                .map(|s| s.as_bytes().to_vec())
                .ok_or_else(|| ToolError::Invalid(format!("not found: {name}")))
        }
        async fn write(
            &self,
            _ctx: &AuthContext,
            _name: &str,
            _bytes: Vec<u8>,
            _content_type: &str,
            _trace_id: Option<&str>,
        ) -> Result<crate::workspace::WorkspaceWrite, ToolError> {
            Err(ToolError::Invalid("read-only".into()))
        }
        async fn append(
            &self,
            _ctx: &AuthContext,
            _name: &str,
            _suffix: &str,
            _content_type: &str,
            _trace_id: Option<&str>,
        ) -> Result<crate::workspace::WorkspaceWrite, ToolError> {
            Err(ToolError::Invalid("read-only".into()))
        }
        async fn delete(
            &self,
            _ctx: &AuthContext,
            _name: &str,
            _trace_id: Option<&str>,
        ) -> Result<(), ToolError> {
            Err(ToolError::Invalid("read-only".into()))
        }
    }

    fn ctx() -> AuthContext {
        AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "u1".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some("t1".into()),
            },
            "org1".into(),
            "t1".into(),
        )
    }

    #[tokio::test]
    async fn tool_reads_default_files_and_reports_missing_ledger() {
        let files = [
            ("report.md", "「市場規模は2兆円を超えると予測された」"),
            ("notes.md", LEDGER),
            ("draft.md", "「市場規模は1兆2000億円に達した」"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let tool = QuoteCheckTool::new(Arc::new(ReadOnly(files)));
        let out = tool
            .call(&ctx(), serde_json::json!({}), None)
            .await
            .unwrap();
        assert!(out
            .content
            .contains("1 件のうち 1 件が notes.md に見つかりません"));
        let out = tool
            .call(&ctx(), serde_json::json!({ "report": "draft.md" }), None)
            .await
            .unwrap();
        assert!(out.content.contains("すべて notes.md に見つかりました"));
        // 台帳が無ければ読み込みエラー（黙って「全部見つかった」にしない）。
        let err = tool
            .call(&ctx(), serde_json::json!({ "ledger": "missing.md" }), None)
            .await;
        assert!(err.is_err());
    }
}
