//! `quote_check`: レポートの引用文字列が証拠台帳に実在するかを機械で確かめる（#409）。
//!
//! 検証を丸ごと機械化はしない。機械が持つのは**「引用した文字列が台帳の逐語引用のどこにも無い」**
//! という一点だけ（幻覚した引用＝最も危険な誤りを拾う）。「その証拠がその主張を支持するか」
//! 「過剰な一般化」は意味の判断なので検証サブエージェントが持つ。
//!
//! - **照合先は台帳の逐語引用**（証拠行 `E12 | … | 逐語引用` の最後の `|` より後ろ）。要約列・
//!   `CONFLICT` 行はモデル自身が書いた文で、そこに一致しても引用の実在にはならない。`|` を
//!   持たない行（書式が崩れた・改行で割れた引用）は行全体を照合先に含める（揺れに強く保つ）。
//! - **正規化**: 空白・約物・括弧を落とし、全角英数を半角へ寄せる。ただし数値の意味を持つ記号
//!   （小数点・符号・`%`）は数字に接していれば残す（「+3.2%」と「32%」を同じにしない）。
//! - **省略**（… ‥ ... ・・・ （中略））で割った断片は、**同じ証拠の中に順に**現れることを求める
//!   （別の証拠の継ぎはぎを通さない）。
//! - 引用の中の証拠 ID（`[E12]`）と Markdown のリンク記法は照合前に外す。入れ子の括弧は内側だけを
//!   引用として扱う（外側は「〜と述べた」を含む地の文のことが多い）。書名の『』は引用として扱わない。

use std::sync::Arc;

use authz::AuthContext;

use crate::tool::{Tool, ToolError, ToolOutcome};
use crate::workspace::WorkspaceStore;

/// これより短い引用（正規化後の字数）は見ない（用語・ラベルの鉤括弧で誤報しないため）。
const MIN_QUOTE_CHARS: usize = 8;
/// 省略で割った断片のうち、これより短いものは照合しない（つなぎの語で誤報しないため）。
/// 照合できる断片が 1 つも無ければ、省略を除いて連結した全体で照合する。
const MIN_FRAGMENT_CHARS: usize = 6;
/// 観測に並べる「見つからなかった引用」の上限（長いレポートでも観測を肥大させない）。
const MAX_LISTED: usize = 30;
/// 観測に載せる 1 件の引用の最大字数（文書由来の巨大な文字列で観測を膨らませない）。
const MAX_LISTED_CHARS: usize = 120;
/// 省略の表記（引用をここで断片に割る）。
const ELLIPSES: [&str; 7] = ["……", "…", "‥", "...", "・・・", "（中略）", "(中略)"];

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

/// 照合用の正規化。空白・約物・括弧を落とし、全角英数記号を半角へ、英字を小文字へ寄せる。
/// 数字に接した `.` `+` `-` `%` は残し、数字に挟まれた `,` は落とす（桁区切り）。
fn normalize(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization as _;
    // NFKC で全角英数・半角カナ・康熙部首（PDF 抽出に混ざる ⼊ ⽇ など）を通常の字形へ寄せる。
    let chars: Vec<char> = text.nfkc().map(fold).collect();
    let digit_at = |i: Option<usize>| {
        i.and_then(|i| chars.get(i))
            .is_some_and(char::is_ascii_digit)
    };
    let mut out = String::with_capacity(text.len());
    for (i, &c) in chars.iter().enumerate() {
        let prev = i.checked_sub(1);
        let next = Some(i + 1);
        let numeric = match c {
            '.' => digit_at(prev) && digit_at(next),
            '+' | '-' => digit_at(next),
            '%' => digit_at(prev),
            _ => false,
        };
        let ignored = c.is_whitespace()
            || c.is_ascii_punctuation()
            || is_cjk_punct(c)
            || matches!(c, '\u{200B}'..='\u{200D}' | '\u{FEFF}');
        if numeric {
            out.push(c);
        } else if !ignored {
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

/// 1 文字の表記ゆれを寄せる（全角 ASCII → 半角・ダッシュ類 → `-`）。
fn fold(c: char) -> char {
    match c as u32 {
        0xFF01..=0xFF5E => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        0x3000 => ' ',
        0x2212 | 0x2010 | 0x2011 | 0x2013 | 0xFE63 => '-',
        _ => c,
    }
}

/// 照合で無視する和文の約物・括弧（書き手ごとに揺れる）。長音符「ー」は文字なので含めない。
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
            | '―'
            | '—'
            | '〜'
            | '～'
            | '｜'
    )
}

/// レポートから引用（「」と “” で囲まれた文字列）を出現順に取り出す。
///
/// 入れ子は内側だけを引用とし、内側を含む外側は捨てる（外側は地の文のことが多い）。
/// 閉じないまま改行した括弧は引用ではない。書名の『』は拾わない。
fn extract_quotes(report: &str) -> Vec<String> {
    let mut quotes = Vec::new();
    // (閉じ括弧, 中身, 入れ子を含んだか)
    let mut stack: Vec<(char, String, bool)> = Vec::new();
    for c in report.chars() {
        if c == '\n' {
            stack.clear();
            continue;
        }
        if let Some(close) = closing(c) {
            if let Some(top) = stack.last_mut() {
                top.2 = true;
            }
            stack.push((close, String::new(), false));
            continue;
        }
        match stack.last_mut() {
            Some((close, _, _)) if c == *close => {
                if let Some((_, text, nested)) = stack.pop() {
                    if !nested {
                        quotes.push(text);
                    }
                }
            }
            Some((_, text, _)) => text.push(c),
            None => {}
        }
    }
    quotes
}

/// 開き括弧 → 閉じ括弧。
fn closing(c: char) -> Option<char> {
    match c {
        '「' => Some('」'),
        '“' => Some('”'),
        _ => None,
    }
}

/// 引用から照合に関係しない書き込み（証拠 ID・Markdown のリンク記法・強調）を外す。
fn strip_markup(quote: &str) -> String {
    let mut out = String::with_capacity(quote.len());
    let mut rest = quote;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let Some(j) = after.find(']') else {
            out.push_str(&rest[i..]);
            rest = "";
            break;
        };
        let inner = &after[..j];
        let tail = &after[j + 1..];
        // [E12] のような証拠 ID は捨てる。[テキスト](url) はテキストだけ残す。
        let is_id = inner.starts_with('E') && inner[1..].chars().all(|c| c.is_ascii_digit());
        if !is_id {
            out.push_str(inner);
        }
        rest = match tail
            .strip_prefix('(')
            .and_then(|t| t.find(')').map(|k| &t[k + 1..]))
        {
            Some(after_url) => after_url,
            None => tail,
        };
    }
    out.push_str(rest);
    out.replace("**", "")
}

/// 台帳から照合先（証拠ごとの逐語引用）を取り出して正規化する。
fn ledger_entries(ledger: &str) -> Vec<String> {
    ledger
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("CONFLICT"))
        .map(|l| match l.rfind('|') {
            // 証拠行は逐語引用の列だけ。要約列はモデル自身が書いた文。
            Some(_) if is_evidence_row(l) => normalize(evidence_quote(l)),
            // 区切りの無い行（崩れた行・改行で割れた引用）は行全体。
            None => normalize(l),
            // それ以外の表（見出し行など）は照合先にしない。
            Some(_) => String::new(),
        })
        .filter(|e| !e.is_empty())
        .collect()
}

/// 証拠行の逐語引用の列。台帳は `証拠ID | URL | 取得日 | 種別 | 要約 | 逐語引用` の 6 列なので、
/// 5 つ目の区切りより後ろを全部とる（引用の中に `|` が混ざっても、最後のセルだけにならない）。
/// 列が足りない行は最後の区切りより後ろ。
fn evidence_quote(line: &str) -> &str {
    match line.splitn(EVIDENCE_COLUMNS, '|').nth(EVIDENCE_COLUMNS - 1) {
        Some(quote) => quote,
        None => line.rfind('|').map_or(line, |i| &line[i + 1..]),
    }
}

/// 台帳の証拠行の列数（skill の書式）。
const EVIDENCE_COLUMNS: usize = 6;

/// `E12 | …` の形の証拠行か。
fn is_evidence_row(line: &str) -> bool {
    let head = line.split('|').next().unwrap_or("").trim();
    head.len() > 1 && head.starts_with('E') && head[1..].chars().all(|c| c.is_ascii_digit())
}

/// 引用が台帳のある 1 つの証拠に、断片の順で現れるか。
fn found_in(quote: &str, entries: &[String]) -> bool {
    let mut parts = vec![strip_markup(quote)];
    for sep in ELLIPSES {
        parts = parts
            .iter()
            .flat_map(|p| p.split(sep).map(str::to_string).collect::<Vec<_>>())
            .collect();
    }
    let normalized: Vec<String> = parts.iter().map(|p| normalize(p)).collect();
    // 数字を含む断片は短くても照合する（「…32%」のような数値の取り違えを素通りさせない）。
    let mut fragments: Vec<&String> = normalized
        .iter()
        .filter(|f| {
            f.chars().count() >= MIN_FRAGMENT_CHARS || f.chars().any(|c| c.is_ascii_digit())
        })
        .collect();
    // 照合できる断片が無ければ、省略を除いて連結した全体で照合する（短い断片だけで素通りさせない）。
    let joined = normalized.concat();
    if fragments.is_empty() {
        fragments.push(&joined);
    }
    entries.iter().any(|entry| {
        let mut from = 0;
        fragments
            .iter()
            .all(|f| match entry[from..].find(f.as_str()) {
                Some(i) => {
                    from += i + f.len();
                    true
                }
                None => false,
            })
    })
}

/// レポートの引用を台帳と突き合わせる。
fn check(report: &str, ledger: &str) -> CheckReport {
    let entries = ledger_entries(ledger);
    let mut seen = std::collections::HashSet::new();
    let mut checked = 0;
    let mut missing = Vec::new();
    for quote in extract_quotes(report) {
        if normalize(&strip_markup(&quote)).chars().count() < MIN_QUOTE_CHARS
            || !seen.insert(quote.clone())
        {
            continue;
        }
        checked += 1;
        if !found_in(&quote, &entries) {
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
            "{report_name} に照合できる引用（{MIN_QUOTE_CHARS} 字以上の「」“”）はありませんでした。"
        );
    }
    if r.missing.is_empty() {
        return format!(
            "{report_name} の引用 {} 件はすべて {ledger_name} の逐語引用に見つかりました（台帳にあることの\
             確認のみ。台帳の引用が原典どおりか・主張を支持するかは別に確かめる）。",
            r.checked
        );
    }
    let mut s = format!(
        "{report_name} の引用 {} 件のうち {} 件が {ledger_name} の逐語引用に見つかりません（空白・約物の\
         違いは無視して照合済み）。機械照合なので誤報もあり得る: 台帳を grep するか原典で確かめ、\
         本当に無いものだけを幻覚・転記ミスとして扱う。\n",
        r.checked,
        r.missing.len()
    );
    for q in r.missing.iter().take(MAX_LISTED) {
        let mut shown: String = q.chars().take(MAX_LISTED_CHARS).collect();
        if q.chars().count() > MAX_LISTED_CHARS {
            shown.push('…');
        }
        let _ = writeln!(s, "- 「{}」", crate::cite::defuse(&shown));
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
        "レポート中の引用（「」“” で囲んだ 8 字以上の文字列）が、証拠台帳の逐語引用（証拠行の\
         最後の列）に実在するかを機械で照合し、見つからない引用だけを返す。空白・約物の違いは無視し、\
         省略（…・（中略））で割った断片は同じ証拠の中に順に現れるかを見る。機械照合なので誤報も\
         あり得る。主張を証拠が支持するかは判定しない（それは読んで確かめる）。"
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
| ID | URL | 取得日 | 種別 | 要約 | 逐語引用 |
E12 | https://example.go.jp/report | 2026-07-30 | 一次 | 国内市場は1.2兆円と発表 | 市場規模は1兆2000億円に達した
E13 | 社内: 就業規則.docx | 2026-10-01 | 一次 | 育休の延長 | 保育所に入所できない場合は、子が1歳6か月に達するまで延長できる
E14 | https://example.go.jp/labor | 2026-08-01 | 一次 | 失業率 | 失業率は前年比+3.2%となった
CONFLICT | E12(1.2兆円) vs E18(9800億円) | 集計範囲が異なる可能性
";

    fn missing(report: &str) -> Vec<String> {
        check(report, LEDGER).missing
    }

    #[test]
    fn finds_quotes_regardless_of_spacing_and_punctuation() {
        let report = "市場は拡大した。「市場規模は 1兆2000億円に達した。」[E12]\n\
                      規程では「保育所に入所できない場合は子が１歳６か月に達するまで延長できる」とある。";
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
        assert!(text.contains("2 件のうち 1 件が notes.md の逐語引用に見つかりません"));
        assert!(text.contains("誤報もあり得る"));
        assert!(text.contains("- 「市場規模は2兆円を超えると予測された」"));
    }

    #[test]
    fn numbers_keep_sign_decimal_point_and_percent() {
        assert!(missing("「失業率は前年比+3.2%となった」").is_empty());
        assert_eq!(missing("「失業率は前年比32%となった」").len(), 1);
        assert_eq!(missing("「失業率は前年比-3.2%となった」").len(), 1);
        // 桁区切りのカンマは落とす。
        let ledger = "E1 | u | d | 一次 | s | 売上は12,000百万円\n";
        assert!(check("「売上は12000百万円」", ledger).missing.is_empty());
    }

    #[test]
    fn summary_column_and_conflict_lines_are_not_evidence() {
        // 要約列（モデルが書いた文）や CONFLICT 行に一致しても実在とは見なさない。
        assert_eq!(missing("「国内市場は1.2兆円と発表」").len(), 1);
        assert_eq!(missing("「集計範囲が異なる可能性」").len(), 1);
    }

    #[test]
    fn ellipsis_fragments_must_appear_in_order_within_one_entry() {
        assert!(
            missing("「保育所に入所できない場合は…1歳6か月に達するまで延長できる」").is_empty()
        );
        assert!(missing("「保育所に入所できない場合は・・・1歳6か月に達するまで」").is_empty());
        assert!(missing("「保育所に入所できない場合は（中略）延長できる」").is_empty());
        // 片方が台帳に無い・順序が逆・別の証拠の継ぎはぎは見つからない扱い。
        assert_eq!(
            missing("「保育所に入所できない場合は…3歳まで延長できる特例がある」").len(),
            1
        );
        assert_eq!(
            missing("「1歳6か月に達するまで延長できる…保育所に入所できない場合は」").len(),
            1
        );
        assert_eq!(
            missing("「市場規模は1兆2000億円に…失業率は前年比+3.2%」").len(),
            1
        );
        // 長い断片と短い数値の断片が混ざっても、数値の断片を照合する。
        assert!(missing("「失業率は前年比…+3.2%」").is_empty());
        assert_eq!(missing("「失業率は前年比…32%」").len(), 1);
        // 断片がどれも短いときは、全体を連結して照合する（素通りさせない）。
        assert_eq!(missing("「日本の…市場は…縮小し…ている」").len(), 1);
    }

    #[test]
    fn pipes_inside_the_quote_column_do_not_hide_the_quote() {
        // 逐語引用の中に `|` が混ざった行（表を写した引用）でも、6 列目以降を全部見る。
        let ledger = "E17 | 社内: 手引き.md | 2026-10-22 | 一次 | 期限表 | | 手続き | 期限 | 提出先 |／| 延長の申出 | 1歳の誕生日の2週間前 | 人事部 |\n";
        let r = check("「| 延長の申出 | 1歳の誕生日の2週間前 | 人事部 |」", ledger);
        assert_eq!((r.checked, r.missing.len()), (1, 0));
        // 要約列（5 列目）には一致させない。
        assert_eq!(
            check("「期限表を手引きから写した」", ledger).missing.len(),
            1
        );
    }

    #[test]
    fn pdf_radical_forms_are_folded() {
        // PDF 抽出で混ざる康熙部首（⼊ U+2F0A・⽇ U+2F47）と通常の漢字を同じに扱う。
        let ledger = "E18 | 社内: 協定.pdf | 2026-10-22 | 一次 | 協定 | ⼊社1年未満の従業員／申出の⽇から1年以内\n";
        assert!(
            check("「入社1年未満の従業員」と「申出の日から1年以内」", ledger)
                .missing
                .is_empty()
        );
    }

    #[test]
    fn markup_inside_quotes_is_ignored() {
        assert!(missing("「市場規模は1兆2000億円に達した[E12]」").is_empty());
        assert!(
            missing("「[市場規模は1兆2000億円に達した](https://example.go.jp/report)」").is_empty()
        );
        assert!(missing("「**市場規模は1兆2000億円に達した**」").is_empty());
    }

    #[test]
    fn nested_quotes_use_the_inner_one_and_titles_are_skipped() {
        // 外側（「〜と述べた」を含む地の文）は捨て、内側の引用だけを照合する。
        let r = check(
            "報告書は「同社は「市場規模は1兆2000億円に達した」と述べた」",
            LEDGER,
        );
        assert_eq!((r.checked, r.missing.len()), (1, 0));
        // 書名の『』は引用として扱わない。
        assert_eq!(
            check("総務省の『令和6年版情報通信白書』によると", LEDGER).checked,
            0
        );
    }

    #[test]
    fn short_terms_and_unclosed_brackets_are_ignored() {
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
    fn long_missing_quotes_are_truncated_in_the_observation() {
        let long = "あ".repeat(500);
        let r = check(&format!("「{long}」"), LEDGER);
        let text = observation("report.md", "notes.md", &r);
        assert!(text.chars().count() < 400, "{}", text.chars().count());
    }

    #[test]
    fn normalize_folds_width_case_punctuation_but_keeps_long_vowel() {
        assert_eq!(normalize("ＡＢＣ　１２３、「x/y」"), "abc123xy");
        assert_eq!(normalize("データ"), "データ");
        assert_eq!(normalize("−3.5％"), "-3.5%");
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
            .contains("1 件のうち 1 件が notes.md の逐語引用に見つかりません"));
        let out = tool
            .call(&ctx(), serde_json::json!({ "report": "draft.md" }), None)
            .await
            .unwrap();
        assert!(out
            .content
            .contains("すべて notes.md の逐語引用に見つかりました"));
        // 台帳が無ければ読み込みエラー（黙って「全部見つかった」にしない）。
        let err = tool
            .call(&ctx(), serde_json::json!({ "ledger": "missing.md" }), None)
            .await;
        assert!(err.is_err());
    }
}
