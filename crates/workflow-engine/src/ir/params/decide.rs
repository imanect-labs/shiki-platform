//! 判定のノード（`ai.decide`・FEAT-1・D-6）の params 契約と出口の規則（ir.md §7.11）。
//!
//! 判定材料（`state`）は信頼しないデータで値式、質問（`questions`）はワークフローの作者が書くコードで
//! リテラルだけ（`$from` は型で弾く）。質問がリテラルなので、出口（ポート）も保存時に静的に決まる。
//!
//! - 質問は内部タグの enum にしない（`deny_unknown_fields` と併用できない）。フラットな struct
//!   （`type`・`options?`・`levels?`・`yes?`・`no?`）にして、[`AiDecideParams::check_cross_fields`] で
//!   種類と項目の整合を見る（control.wait と同じ流儀）。
//! - 出口の規則（[`AiDecideParams::ports`]）は保存時検証 V2・検証 API の `node_ports`・実行時の振り分け
//!   （[`AiDecideParams::port_for`]）が同じものを使う（web に導出を書き写さない・PortRule::DecideRoute）。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::ParamsIssue;
use crate::ir::expr::ValueExpr;

/// 質問の数の上限。
pub const MAX_DECIDE_QUESTIONS: usize = 20;
/// 選択肢の数（下限・上限）。
pub const MIN_DECIDE_OPTIONS: usize = 2;
pub const MAX_DECIDE_OPTIONS: usize = 100;
/// 段階の数（下限・上限）。
pub const MIN_DECIDE_LEVELS: usize = 2;
pub const MAX_DECIDE_LEVELS: usize = 10;
/// 問い（`instructions`）の上限（文字数）。
pub const MAX_DECIDE_INSTRUCTIONS_CHARS: usize = 2000;
/// 選択肢・はい/いいえの説明の上限（文字数）。選択肢に名前があるときは、AI への手がかり（名前と説明を
/// つないだもの・[`DecideOption::ai_hint`]）をこの長さに収める。
pub const MAX_DECIDE_DESCRIPTION_CHARS: usize = 500;
/// 選択肢の名前（`label`）の上限（文字数・段階の名前と同じ）。
pub const MAX_DECIDE_OPTION_LABEL_CHARS: usize = 100;
/// 段階の名前の上限（文字数）。
pub const MAX_DECIDE_LEVEL_CHARS: usize = 100;
/// キー（質問・選択肢）の上限（文字数）。
pub const MAX_DECIDE_KEY_CHARS: usize = 64;
/// 判定材料（JSON に直した長さ）の上限。実行時に確かめる（`decision_state_too_large`）。
pub const MAX_DECIDE_STATE_BYTES: usize = 32 * 1024;
/// しきい値（`min_confidence`）の範囲。
pub const MIN_DECIDE_CONFIDENCE: f64 = 0.5;
pub const MAX_DECIDE_CONFIDENCE: f64 = 1.0;

/// 確からしさがしきい値に届かなかったときの出口。
pub const UNCERTAIN_PORT: &str = "uncertain";

/// 選択肢のキーにできない名前（出口の名前と取り違えないため。構造ポート・他のノードの出口の名前）。
pub const RESERVED_DECIDE_KEYS: &[&str] = &[
    "out",
    UNCERTAIN_PORT,
    "error",
    "default",
    "timeout",
    "true",
    "false",
];

/// 質問の種類（答えの値域）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum DecideQuestionType {
    /// 選択肢から 1 つ（`options`）。
    Choice,
    /// 段階から 1 つ（`levels`・低い順）。
    Score,
    /// はい / いいえ（`yes`・`no` はそれぞれに当たる場合の説明・省略可）。
    Boolean,
}

/// 選択肢 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DecideOption {
    /// 答えの値（`^[a-z][a-z0-9_]{0,63}$`・質問の中で一意・予約名は不可）。値で振り分けるときは出口の名前になる。
    pub key: String,
    /// 選択肢の名前（利用者に見せる名前・答えの `label`・テーブルの選択肢の列に書き込む値・100 字以内・質問の
    /// 中で一意・省略可・D-87）。省略した定義は、今までどおり説明かキーを答えの名前にする（[`Self::name`]）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub label: Option<String>,
    /// どんなときにこの選択肢か（AI への手がかり・500 字以内・省略可）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
}

/// 空白だけの文字は書いていないものとして扱う。
fn written(s: Option<&str>) -> Option<&str> {
    s.filter(|v| !v.trim().is_empty())
}

impl DecideOption {
    /// 答えの名前（出力の `answers.<キー>.label`）。名前が無ければ、名前を足す前の定義と同じく説明、それも
    /// 無ければキー（保存済みの定義の後ろのブロックが読む値を変えない・D-87）。
    #[must_use]
    pub fn name(&self) -> &str {
        written(self.label.as_deref())
            .or_else(|| written(self.description.as_deref()))
            .unwrap_or(&self.key)
    }

    /// AI への手がかり（判定のプロンプトで選択肢のキーの横に書く文）。名前と説明の両方があれば「名前: 説明」、
    /// 片方だけならそれ、どちらも無ければ空。名前の無い定義は説明だけなので、名前を足す前と同じ文になる
    /// （プロンプトの版と、決定キャッシュのキーを変えない・D-87）。
    #[must_use]
    pub fn ai_hint(&self) -> String {
        match (
            written(self.label.as_deref()),
            written(self.description.as_deref()),
        ) {
            (Some(label), Some(description)) => format!("{}: {description}", label.trim()),
            (Some(label), None) => label.trim().to_string(),
            // 説明だけのときは、名前を足す前と同じく書いたままを渡す。
            (None, _) => self.description.clone().unwrap_or_default(),
        }
    }
}

/// 質問 1 件（リテラル）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DecideQuestion {
    /// 答えのキー（`^[a-z][a-z0-9_]{0,63}$`・ノードの中で一意）。後続は `/answers/<key>` で読む。
    pub key: String,
    /// 質問の種類。
    #[serde(rename = "type")]
    #[ts(rename = "type")]
    pub kind: DecideQuestionType,
    /// AI に尋ねること（2000 字までの自然文）。空のまま保存はでき、入れるまで実行と自動実行のオンを止める
    /// （readiness）。実行時に空なら AI を呼ばずに失敗する（[`DECIDE_INSTRUCTIONS_MISSING`]・D-108）。
    pub instructions: String,
    /// choice の選択肢（2〜100 件）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub options: Option<Vec<DecideOption>>,
    /// score の段階（低い順・2〜10 件・一意）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub levels: Option<Vec<String>>,
    /// boolean で「はい」に当たる場合の説明（省略可）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub yes: Option<String>,
    /// boolean で「いいえ」に当たる場合の説明（省略可）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub no: Option<String>,
}

/// 振り分けの仕方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum DecideRouteBy {
    /// 答えの値で分ける（選択肢のキー・`true` / `false`）。しきい値に届かなければ `uncertain`（既定）。
    #[default]
    Value,
    /// 確かなときだけ進める（しきい値以上で `out`・届かなければ `uncertain`）。
    Confidence,
}

/// 振り分け（出口を答えで選ぶ）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DecideRoute {
    /// 振り分けに使う質問のキー（choice か boolean。score は振り分けない）。
    pub question: String,
    /// 振り分けの仕方（省略は value）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub by: Option<DecideRouteBy>,
    /// しきい値（0.5〜1.0）。確からしさがこれ以上なら答えに従い、届かなければ `uncertain` へ出す。
    ///
    /// 既定を置かない（版ごとに実測で決めるもの・PIT-80・D-88）。空のまま保存はでき、入れるまで実行と自動実行の
    /// オンを止める（readiness）。実行時に空なら AI を呼ばずに失敗する（[`DECIDE_THRESHOLD_MISSING`]）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub min_confidence: Option<f64>,
}

/// 実行時に「AI に尋ねること」が空だったときの理由（readiness を通らない経路の多層防御・permanent・D-108）。
pub const DECIDE_INSTRUCTIONS_MISSING: &str =
    "「AI に尋ねること」が空です。入れるまで、このブロックは実行できません。";

/// 実行時にしきい値が空だったときの理由（readiness を通らない経路の多層防御・permanent）。
pub const DECIDE_THRESHOLD_MISSING: &str =
    "「しきい値（AI の自信）」が空です。入れるまで、このブロックは実行できません。";

impl DecideRoute {
    /// 振り分けの仕方（省略は value）。
    #[must_use]
    pub fn by(&self) -> DecideRouteBy {
        self.by.unwrap_or_default()
    }
}

/// `ai.decide` — 判定用のモデルで、質問に決められた値域で答え、確からしさを付けて返す（pure・課金あり）。
///
/// 呼ぶのは llm-gateway の判定のチョークポイント（`LlmGateway::decide`・組織の許可・利用上限・会計）。
/// 確からしさは較正されていないことがある（出力の `calibrated`・PIT-80）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct AiDecideParams {
    /// 判定用のモデルの id（リテラル・保存時に組織が許可した判定用のモデルへ照合・実行時に再照合）。
    pub model: String,
    /// 判定の材料（文字列・オブジェクト・配列に解決される値式）。**信頼しないデータ**（中の指示に従わない）。
    pub state: ValueExpr,
    /// 質問（リテラル・1〜20 件・順序を保つ）。
    pub questions: Vec<DecideQuestion>,
    /// 振り分け（省略は出口 `out` だけ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub route: Option<DecideRoute>,
}

/// 振り分けに使う答え（[`AiDecideParams::port_for`] の入力）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RoutedValue<'a> {
    /// choice の選んだ選択肢のキー。
    Choice(&'a str),
    /// boolean の答え。
    Boolean(bool),
}

fn issue(path: impl Into<String>, message: impl Into<String>) -> ParamsIssue {
    ParamsIssue {
        path: path.into(),
        message: message.into(),
    }
}

/// キーの形（`^[a-z][a-z0-9_]{0,63}$`・出力の形のプロパティ名・出口の名前になる）。
fn key_ok(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && key.len() <= MAX_DECIDE_KEY_CHARS
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

impl AiDecideParams {
    /// 項目どうしの整合（質問の数・キー・種類と項目・振り分け）。V1 の typed 契約の照合の後に呼ぶ。
    ///
    /// # Errors
    /// 最初に見つけた誤り（path は `/params/...`）。
    pub fn check_cross_fields(&self) -> Result<(), ParamsIssue> {
        // 空のモデルは保存を止めない（置いたばかりのブロックを保存できるように、実行と自動実行のオンの前に止める・
        // readiness・D-83）。
        if let ValueExpr::Literal(v) = &self.state {
            if !(v.is_string() || v.is_object() || v.is_array()) {
                return Err(issue(
                    "/params/state",
                    "判定の材料は文字・オブジェクト・配列のどれかにしてください",
                ));
            }
        }
        if self.questions.is_empty() || self.questions.len() > MAX_DECIDE_QUESTIONS {
            return Err(issue(
                "/params/questions",
                format!("質問は 1〜{MAX_DECIDE_QUESTIONS} 件にしてください"),
            ));
        }
        let mut keys = std::collections::BTreeSet::new();
        for (i, q) in self.questions.iter().enumerate() {
            let at = format!("/params/questions/{i}");
            if !key_ok(&q.key) {
                return Err(issue(
                    format!("{at}/key"),
                    "質問のキーは英小文字で始め、英小文字・数字・_ だけの 64 字以内にしてください",
                ));
            }
            if !keys.insert(q.key.as_str()) {
                return Err(issue(
                    format!("{at}/key"),
                    format!("質問のキー「{}」が重なっています", q.key),
                ));
            }
            // 空の問いは保存を止めない（置いたばかりのブロックを、問いを書く前に保存できるように、実行と自動実行の
            // オンの前に止める・readiness・D-108）。
            if chars(&q.instructions) > MAX_DECIDE_INSTRUCTIONS_CHARS {
                return Err(issue(
                    format!("{at}/instructions"),
                    format!("問いは {MAX_DECIDE_INSTRUCTIONS_CHARS} 字以内にしてください"),
                ));
            }
            check_kind(q, &at)?;
        }
        if let Some(route) = &self.route {
            self.check_route(route)?;
        }
        Ok(())
    }

    fn check_route(&self, route: &DecideRoute) -> Result<(), ParamsIssue> {
        let Some(q) = self.question(&route.question) else {
            return Err(issue(
                "/params/route/question",
                format!("振り分けに使う質問「{}」がありません", route.question),
            ));
        };
        if q.kind == DecideQuestionType::Score {
            return Err(issue(
                "/params/route/question",
                "段階（score）の質問では振り分けられません（選択肢かはい/いいえの質問を選ぶか、\
                 後ろに条件分岐を置いて /answers/<キー>/level_index で分けてください）",
            ));
        }
        // 空のしきい値は保存を止めず、実行と自動実行のオンの前に止める（readiness・D-88）。
        let Some(c) = route.min_confidence else {
            return Ok(());
        };
        if !c.is_finite() || !(MIN_DECIDE_CONFIDENCE..=MAX_DECIDE_CONFIDENCE).contains(&c) {
            return Err(issue(
                "/params/route/min_confidence",
                format!(
                    "しきい値は {MIN_DECIDE_CONFIDENCE:.2}〜{MAX_DECIDE_CONFIDENCE:.2} にしてください。"
                ),
            ));
        }
        Ok(())
    }

    /// キーで質問を引く。
    #[must_use]
    pub fn question(&self, key: &str) -> Option<&DecideQuestion> {
        self.questions.iter().find(|q| q.key == key)
    }

    /// 出口（ポート）の一覧（`error` などの構造ポートは含めない・順序は表示の順）。
    ///
    /// 出口を決めるのに要る項目（振り分けと、振り分けに使う質問の種類・選択肢のキー）だけを確かめる。ほかの質問の
    /// 誤り（書きかけの問いなど）・モデルの未選択・しきい値の範囲では `None` にしない（エディタは出口を描き続け、
    /// 利用者が古い出口へつながない・V1 がそれらを報告する）。振り分けの質問が無い・同じキーの質問が複数ある・
    /// 振り分けられない種類・選択肢のキーが出口の名前にできない（形・予約名・重なり・件数）なら `None`
    /// （V2 は検査しない・V1 が報告する）。
    #[must_use]
    pub fn ports(&self) -> Option<Vec<String>> {
        let Some(route) = &self.route else {
            return Some(vec!["out".to_string()]);
        };
        let mut found = self.questions.iter().filter(|q| q.key == route.question);
        let q = found.next()?;
        if found.next().is_some() {
            return None;
        }
        let mut ports: Vec<String> = match (route.by(), q.kind) {
            // 段階（score）の質問では振り分けない（どちらの分け方でも V1 が拒否する）。
            (_, DecideQuestionType::Score) => return None,
            (DecideRouteBy::Confidence, _) => vec!["out".to_string()],
            (DecideRouteBy::Value, DecideQuestionType::Choice) => {
                // 選択肢のキーが出口の名前になるので、V1 と同じ決まり（形・予約名・重なり・件数）を満たすこと
                // （説明の長さは出口に関わらないので見ない）。
                let options = q.options.as_deref().unwrap_or_default();
                let mut seen = std::collections::BTreeSet::new();
                let named = (MIN_DECIDE_OPTIONS..=MAX_DECIDE_OPTIONS).contains(&options.len())
                    && options.iter().all(|o| {
                        key_ok(&o.key)
                            && !RESERVED_DECIDE_KEYS.contains(&o.key.as_str())
                            && seen.insert(o.key.as_str())
                    });
                if !named {
                    return None;
                }
                options.iter().map(|o| o.key.clone()).collect()
            }
            (DecideRouteBy::Value, DecideQuestionType::Boolean) => {
                vec!["true".to_string(), "false".to_string()]
            }
        };
        ports.push(UNCERTAIN_PORT.to_string());
        Some(ports)
    }

    /// 答えから出口を選ぶ（[`Self::ports`] と同じ規則）。確からしさはしきい値**以上**で答えに従う
    /// （較正されていない確からしさでも同じ規則・H4）。振り分けが無ければ `out`。
    #[must_use]
    pub fn port_for(&self, routed: Option<(RoutedValue<'_>, f64)>) -> String {
        let (Some(route), Some((value, confidence))) = (&self.route, routed) else {
            return "out".to_string();
        };
        // 非有限（NaN）は届かなかったものとして扱う（答えの検査を通った値だけが来るが、多層防御）。しきい値が空なら
        // 答えに従わない（実行の前に止めるが、多層防御で人の確認へ回す・D-88）。
        let Some(min) = route.min_confidence else {
            return UNCERTAIN_PORT.to_string();
        };
        if confidence.is_nan() || confidence < min {
            return UNCERTAIN_PORT.to_string();
        }
        match (route.by(), value) {
            (DecideRouteBy::Confidence, _) => "out".to_string(),
            (DecideRouteBy::Value, RoutedValue::Choice(key)) => key.to_string(),
            (DecideRouteBy::Value, RoutedValue::Boolean(b)) => b.to_string(),
        }
    }
}

/// 種類と項目の整合（choice に levels を書く・score に options を書く等は拒否する）。
fn check_kind(q: &DecideQuestion, at: &str) -> Result<(), ParamsIssue> {
    let stray = |field: &str, kind: &str| {
        issue(
            format!("{at}/{field}"),
            format!("項目 '{field}' は {kind} の質問では使えません"),
        )
    };
    match q.kind {
        DecideQuestionType::Choice => {
            if q.levels.is_some() {
                return Err(stray("levels", "選択肢（choice）"));
            }
            for (field, v) in [("yes", &q.yes), ("no", &q.no)] {
                if v.is_some() {
                    return Err(stray(field, "選択肢（choice）"));
                }
            }
            check_options(q.options.as_deref(), at)
        }
        DecideQuestionType::Score => {
            if q.options.is_some() {
                return Err(stray("options", "段階（score）"));
            }
            for (field, v) in [("yes", &q.yes), ("no", &q.no)] {
                if v.is_some() {
                    return Err(stray(field, "段階（score）"));
                }
            }
            check_levels(q.levels.as_deref(), at)
        }
        DecideQuestionType::Boolean => {
            if q.options.is_some() {
                return Err(stray("options", "はい/いいえ（boolean）"));
            }
            if q.levels.is_some() {
                return Err(stray("levels", "はい/いいえ（boolean）"));
            }
            for (field, v) in [("yes", &q.yes), ("no", &q.no)] {
                if v.as_deref()
                    .is_some_and(|s| chars(s) > MAX_DECIDE_DESCRIPTION_CHARS)
                {
                    return Err(issue(
                        format!("{at}/{field}"),
                        format!("説明は {MAX_DECIDE_DESCRIPTION_CHARS} 字以内にしてください"),
                    ));
                }
            }
            Ok(())
        }
    }
}

fn check_options(options: Option<&[DecideOption]>, at: &str) -> Result<(), ParamsIssue> {
    let options = options.unwrap_or_default();
    if !(MIN_DECIDE_OPTIONS..=MAX_DECIDE_OPTIONS).contains(&options.len()) {
        return Err(issue(
            format!("{at}/options"),
            format!("選択肢は {MIN_DECIDE_OPTIONS}〜{MAX_DECIDE_OPTIONS} 件にしてください"),
        ));
    }
    let mut keys = std::collections::BTreeSet::new();
    let mut labels = std::collections::BTreeSet::new();
    for (j, o) in options.iter().enumerate() {
        let path = format!("{at}/options/{j}");
        if !key_ok(&o.key) {
            return Err(issue(
                format!("{path}/key"),
                "選択肢のキーは英小文字で始め、英小文字・数字・_ だけの 64 字以内にしてください",
            ));
        }
        if RESERVED_DECIDE_KEYS.contains(&o.key.as_str()) {
            return Err(issue(
                format!("{path}/key"),
                format!(
                    "選択肢のキーに「{}」は使えません（出口の名前と取り違えるため。使えない名前: {}）",
                    o.key,
                    RESERVED_DECIDE_KEYS.join(", ")
                ),
            ));
        }
        if !keys.insert(o.key.as_str()) {
            return Err(issue(
                format!("{path}/key"),
                format!("選択肢のキー「{}」が重なっています", o.key),
            ));
        }
        if o.description
            .as_deref()
            .is_some_and(|d| chars(d) > MAX_DECIDE_DESCRIPTION_CHARS)
        {
            return Err(issue(
                format!("{path}/description"),
                format!("選択肢の説明は {MAX_DECIDE_DESCRIPTION_CHARS} 字以内にしてください"),
            ));
        }
        check_option_label(o, &path, &mut labels)?;
    }
    Ok(())
}

/// 選択肢の名前（D-87）: 100 字まで・質問の中で重ならない・説明とつないだ AI への手がかりが説明の上限に収まる。
/// 空白だけの名前は書いていないものとして扱う（今までどおり説明かキーを名前にする）。
fn check_option_label<'a>(
    o: &'a DecideOption,
    path: &str,
    labels: &mut std::collections::BTreeSet<&'a str>,
) -> Result<(), ParamsIssue> {
    let Some(label) = written(o.label.as_deref()).map(str::trim) else {
        return Ok(());
    };
    if chars(label) > MAX_DECIDE_OPTION_LABEL_CHARS {
        return Err(issue(
            format!("{path}/label"),
            format!("「選択肢の名前」は {MAX_DECIDE_OPTION_LABEL_CHARS} 字までにしてください。"),
        ));
    }
    if !labels.insert(label) {
        return Err(issue(
            format!("{path}/label"),
            format!(
                "選択肢の名前「{label}」はほかの選択肢でも使っています。別の名前にしてください。"
            ),
        ));
    }
    if chars(&o.ai_hint()) > MAX_DECIDE_DESCRIPTION_CHARS {
        return Err(issue(
            format!("{path}/description"),
            format!(
                "「選択肢の名前」と「AI への説明」は、合わせて {} 字までにしてください。",
                MAX_DECIDE_DESCRIPTION_CHARS - 2
            ),
        ));
    }
    Ok(())
}

fn check_levels(levels: Option<&[String]>, at: &str) -> Result<(), ParamsIssue> {
    let levels = levels.unwrap_or_default();
    if !(MIN_DECIDE_LEVELS..=MAX_DECIDE_LEVELS).contains(&levels.len()) {
        return Err(issue(
            format!("{at}/levels"),
            format!("段階は {MIN_DECIDE_LEVELS}〜{MAX_DECIDE_LEVELS} 件にしてください"),
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for (j, l) in levels.iter().enumerate() {
        let len = chars(l.trim());
        if len == 0 || chars(l) > MAX_DECIDE_LEVEL_CHARS {
            return Err(issue(
                format!("{at}/levels/{j}"),
                format!("段階の名前は 1〜{MAX_DECIDE_LEVEL_CHARS} 字にしてください"),
            ));
        }
        if !seen.insert(l.as_str()) {
            return Err(issue(
                format!("{at}/levels/{j}"),
                format!("段階「{l}」が重なっています"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
