//! 予算ガード（Task 5.7）。
//!
//! 長ホライズンの暴走を防ぐ: **最大ステップ・最大経過時間・トークン上限・コスト上限**を設定し、
//! llm-gateway のトークン会計（`Usage`）と連動して累積、超過時に安全停止する。上限接近では
//! 警告（[`BudgetCheck::Warn`]）を出し、UI/承認（5.6）で続行可否を促せるようにする。
//!
//! 判定は**純粋関数**（[`Budget::check`]）に閉じ、単体＋proptest で網羅する。時刻は呼び出し側が
//! [`Instant`] を渡す（テスト可能性・決定性のため内部で now を取らない）。

use std::time::Instant;

use llm_gateway::{Block, Message};

use crate::vocab::ToolName;

/// 予算の種別（どの軸で警告/超過したか）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BudgetKind {
    Steps,
    Time,
    Tokens,
    Cost,
}

impl BudgetKind {
    /// 監査・UI 表示用の安定文字列。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            BudgetKind::Steps => "steps",
            BudgetKind::Time => "time",
            BudgetKind::Tokens => "tokens",
            BudgetKind::Cost => "cost",
        }
    }
}

/// 累積消費（1 セッション）。チェックポイントに載るため直列化可能。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Spent {
    pub steps: usize,
    /// **課金の累計**。各ステップの prompt＋completion をそのまま足す。prompt には毎回
    /// 履歴全体が含まれるので、実際の請求と同じくステップ数に対して ≒二次で伸びる。
    /// コスト会計・Langfuse に出すのはこちら。
    pub tokens: u64,
    /// **仕事量の累計**（上限判定に使う）。そのステップで**新しく生まれたトークン**だけを
    /// 足す ＝ completion ＋ 前ステップから増えた prompt 分（＝新しいツール結果）。
    ///
    /// 上限を `tokens` で見ると、履歴の再送を「消費」として何度も数えるため、上限値が
    /// 「何件調べられるか」と桁でずれる。実 LLM では 1 体 12 万でも 20 万でも調査
    /// サブエージェントが毎回上限で死に、委譲機構が使われないままだった（#404）。
    /// 請求は `tokens`、できる仕事の量は `fresh_tokens`、と役割を分ける。
    pub fresh_tokens: u64,
    /// 直前ステップの prompt トークン（`fresh_tokens` の差分計算に使う内部状態）。
    prev_prompt_tokens: u64,
    pub cost_usd_micros: i64,
}

impl Spent {
    /// 1 ステップ分の消費を足し込む（step は +1）。
    ///
    /// `prompt_tokens` を分けて受けるのは `fresh_tokens`（新規ぶん）を出すため。prompt は
    /// 履歴全体なので単調増加し、その**増分**がそのステップで新しく積まれた内容
    /// （直前の completion ＋ ツール結果）にあたる。剪定で縮んだときは 0 とみなす。
    pub fn add_step(&mut self, prompt_tokens: u64, completion_tokens: u64, cost_usd_micros: i64) {
        self.steps = self.steps.saturating_add(1);
        self.tokens = self
            .tokens
            .saturating_add(prompt_tokens.saturating_add(completion_tokens));
        let grew = prompt_tokens.saturating_sub(self.prev_prompt_tokens);
        self.prev_prompt_tokens = prompt_tokens;
        self.fresh_tokens = self
            .fresh_tokens
            .saturating_add(grew.saturating_add(completion_tokens));
        self.cost_usd_micros = self.cost_usd_micros.saturating_add(cost_usd_micros);
    }

    /// ツールの中で起きた LLM 消費を足し込む（**step は増やさない**・#391）。
    ///
    /// サブエージェント委譲（`subagent`）は 1 ツール呼び出しの内側で子ループを回すため、その
    /// トークン/コストは親の会計に現れない。ここで積むことで**親の `Budget` が子の消費込みで
    /// 止まる**（トークンが十数倍になり得る機構の唯一の安全弁）。steps は親のループ回数を表す
    /// 指標なので触らない。
    pub fn add_external(&mut self, tokens: u64, fresh_tokens: u64, cost_usd_micros: i64) {
        self.tokens = self.tokens.saturating_add(tokens);
        self.fresh_tokens = self.fresh_tokens.saturating_add(fresh_tokens);
        self.cost_usd_micros = self.cost_usd_micros.saturating_add(cost_usd_micros);
    }

    /// 直前ステップでプロバイダが報告した prompt トークン（剪定の概算補正に使う・#529）。
    #[must_use]
    pub fn last_prompt_tokens(&self) -> u64 {
        self.prev_prompt_tokens
    }
}

/// 予算上限。`None` は当該軸を無制限にする（`Budget::chat` は token/cost を付けない。通常チャットの
/// 上限は chat ワーカーが設定値から入れる・#426）。
#[derive(Debug, Clone)]
pub struct Budget {
    /// LLM 呼び出し回数の上限（安全停止・必須）。
    pub max_steps: usize,
    /// 全体デッドライン（超えたらステップ境界で停止）。
    pub deadline: Option<Instant>,
    /// run 開始時刻（`deadline` があるとき経過割合で時間警告を出すために保持）。
    pub started_at: Option<Instant>,
    /// 累積トークン（prompt+completion）上限。
    pub max_tokens: Option<u64>,
    /// 累積コスト（マイクロ USD）上限。
    pub max_cost_usd_micros: Option<i64>,
    /// 警告を出す割合（0.0..=1.0）。既定 0.8。上限の当該割合到達で `Warn`。
    pub warn_at_fraction: f64,
}

impl Budget {
    /// Chat プロファイル既定（短ホライズン・token/cost は呼び出し側が入れる・現行挙動と互換）。
    #[must_use]
    pub fn chat(max_steps: usize) -> Self {
        Budget {
            max_steps,
            deadline: None,
            started_at: None,
            max_tokens: None,
            max_cost_usd_micros: None,
            warn_at_fraction: 0.8,
        }
    }

    /// Autonomous プロファイル既定（長ホライズン・全軸に上限）。
    #[must_use]
    pub fn autonomous(
        max_steps: usize,
        deadline: Option<Instant>,
        max_tokens: u64,
        max_cost_usd_micros: i64,
    ) -> Self {
        Budget {
            max_steps,
            deadline,
            // deadline があるときのみ開始時刻を控える（経過割合で時間警告を出すため）。
            started_at: deadline.map(|_| Instant::now()),
            max_tokens: Some(max_tokens),
            max_cost_usd_micros: Some(max_cost_usd_micros),
            warn_at_fraction: 0.8,
        }
    }

    /// 現在の消費・時刻に対する予算判定。**次のステップに入る前**に呼ぶ。
    ///
    /// 超過は `Exceeded`（安全停止）、上限の `warn_at_fraction` 到達は `Warn`（続行可）、
    /// いずれも無ければ `Ok`。複数軸が同時該当なら「超過 > 警告」かつ steps→time→tokens→cost の順で 1 つ返す。
    // cost は `.max(0)` 後に u64 化するため符号消失は起きない（金額は非負）。
    #[allow(clippy::cast_sign_loss)]
    #[must_use]
    pub fn check(&self, spent: &Spent, now: Instant) -> BudgetCheck {
        // --- 超過（安全停止）を最優先で検出する。 ---
        if spent.steps >= self.max_steps {
            return BudgetCheck::Exceeded(BudgetKind::Steps);
        }
        if self.deadline.is_some_and(|d| now >= d) {
            return BudgetCheck::Exceeded(BudgetKind::Time);
        }
        if let Some(max) = self.max_tokens {
            // 上限は**新規ぶん**で見る（履歴の再送を仕事量として二重計上しない・#404）。
            if spent.fresh_tokens >= max {
                return BudgetCheck::Exceeded(BudgetKind::Tokens);
            }
        }
        if let Some(max) = self.max_cost_usd_micros {
            if spent.cost_usd_micros >= max {
                return BudgetCheck::Exceeded(BudgetKind::Cost);
            }
        }
        // --- 警告（上限接近）。steps→time→tokens→cost の順で最初の 1 軸。 ---
        let frac = self.warn_at_fraction.clamp(0.0, 1.0);
        if fraction_reached(spent.steps as u64, self.max_steps as u64, frac) {
            return BudgetCheck::Warn(BudgetKind::Steps, spent.steps as u64, self.max_steps as u64);
        }
        // 時間: 開始からの経過が deadline までの割合で frac に達したら警告（秒単位・突然停止を避ける）。
        if let (Some(start), Some(deadline)) = (self.started_at, self.deadline) {
            let total = deadline.saturating_duration_since(start).as_secs();
            let elapsed = now.saturating_duration_since(start).as_secs();
            if fraction_reached(elapsed, total, frac) {
                return BudgetCheck::Warn(BudgetKind::Time, elapsed, total);
            }
        }
        if let Some(max) = self.max_tokens {
            if fraction_reached(spent.fresh_tokens, max, frac) {
                return BudgetCheck::Warn(BudgetKind::Tokens, spent.fresh_tokens, max);
            }
        }
        if let Some(max) = self.max_cost_usd_micros {
            let (used, lim) = (spent.cost_usd_micros.max(0) as u64, max.max(0) as u64);
            if fraction_reached(used, lim, frac) {
                return BudgetCheck::Warn(BudgetKind::Cost, used, lim);
            }
        }
        BudgetCheck::Ok
    }

    /// 残り予算を出す（**モデルへ見せる**ための値・#407）。
    ///
    /// 上限判定と同じ軸（ステップ・`fresh_tokens`・deadline）で数える。判定と表示がずれると
    /// 「まだ余裕がある」と言った直後に切られることになるため、`check` と同じ値を使う。
    // cost は表示に使わない（モデルに単価の判断材料が無く、行動が変わらないため）。
    #[allow(clippy::cast_precision_loss)]
    #[must_use]
    pub fn remaining(&self, spent: &Spent, now: Instant) -> Remaining {
        let steps = self.max_steps.saturating_sub(spent.steps);
        let tokens = self
            .max_tokens
            .map(|m| m.saturating_sub(spent.fresh_tokens));
        let seconds = self
            .deadline
            .map(|d| d.saturating_duration_since(now).as_secs());
        let tight = |left: u64, max: u64| max > 0 && (left as f64) < max as f64 * LANDING_FRACTION;
        let landing = steps <= LANDING_STEPS
            || tokens.is_some_and(|t| tight(t, self.max_tokens.unwrap_or(0)))
            || match (seconds, self.started_at, self.deadline) {
                (Some(left), Some(start), Some(deadline)) => {
                    tight(left, deadline.saturating_duration_since(start).as_secs())
                }
                _ => false,
            };
        Remaining {
            steps,
            tokens,
            seconds,
            landing,
        }
    }
}

/// 作成系の道具を初めて呼んだときに 1 回だけ広げる予算（D-53 の HD-4 (c)・D-204）。
///
/// 通常の会話の予算（既定 6 手）は、道具を探す・スキーマを読む・作る・直して送り直す・答える、で尽きる。
/// 全体を上げると全会話の最悪の費用が上がるので、作成系の道具（`on_tools`）を呼んだ run だけを広げる。
///
/// - **広げたかどうかは保存しない。** 毎ステップ、履歴（[`crate::Checkpoint::messages`]）に `on_tools` の
///   呼び出し（`Block::ToolUse`）があるかから導く（[`Self::called`]）。チェックポイントの形を変えずに、再開・
///   古いチェックポイント・takeover でも広げた状態がずれない（再開した直後に元の手数へ戻らない）。
/// - 失敗・却下された呼び出しも「呼んだ」に数える（直して送り直す手数が要るのはむしろそのとき）。
///   `tool_search` で定義を読み込んだだけでは広げない（呼び出しは `tool_search` のもの）。
/// - 上限は固定値なので、何度呼んでも広がるのは 1 回ぶん（プロンプト注入で呼ばせても、それ以上は増えない）。
/// - 広げるのは手数・トークン（新規ぶん）・費用・1 応答の出力上限だけ。元より小さい値には下げず、上限の
///   無い軸は無いままにする。時間の上限・思考強度・system は変えない（キャッシュと一貫性のため）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetExtension {
    /// 呼んだら広げる道具。
    pub on_tools: Vec<ToolName>,
    /// 広げた後の最大ステップ。
    pub max_steps: usize,
    /// 広げた後の累積トークン上限（新規ぶん＝[`Spent::fresh_tokens`] の軸）。
    pub max_fresh_tokens: u64,
    /// 広げた後の累積コスト上限（マイクロ USD）。
    pub max_cost_usd_micros: i64,
    /// 広げた後の 1 応答の出力上限（`max_tokens`）。
    pub max_response_tokens: u32,
}

impl BudgetExtension {
    /// 履歴に `on_tools` の呼び出しがあるか（広げるかの判定・毎ステップ導く）。
    #[must_use]
    pub fn called(&self, messages: &[Message]) -> bool {
        messages.iter().flat_map(|m| &m.content).any(|b| match b {
            Block::ToolUse { name, .. } => self.on_tools.iter().any(|t| t.as_str() == name),
            _ => false,
        })
    }

    /// 広げた予算。元より小さい値には下げず、上限の無い軸（`None`）は無いままにする。
    #[must_use]
    pub fn extend(&self, budget: &Budget) -> Budget {
        Budget {
            max_steps: budget.max_steps.max(self.max_steps),
            max_tokens: budget.max_tokens.map(|m| m.max(self.max_fresh_tokens)),
            max_cost_usd_micros: budget
                .max_cost_usd_micros
                .map(|m| m.max(self.max_cost_usd_micros)),
            ..budget.clone()
        }
    }

    /// 広げた 1 応答の出力上限（元より小さくはしない）。
    #[must_use]
    pub fn extend_response_tokens(&self, max_tokens: Option<u32>) -> Option<u32> {
        Some(max_tokens.map_or(self.max_response_tokens, |m| {
            m.max(self.max_response_tokens)
        }))
    }
}

/// `used` が `limit * frac` 以上か（limit=0 は「無制限扱い」で常に false）。
// 予算のトークン/コストは実運用で 2^52 に達しないため、割合判定の f64 変換で精度は問題にならない。
#[allow(clippy::cast_precision_loss)]
fn fraction_reached(used: u64, limit: u64, frac: f64) -> bool {
    if limit == 0 {
        return false;
    }
    // 整数で threshold = ceil(limit * frac) 相当。浮動小数は割合換算にのみ使う。
    let threshold = (limit as f64 * frac).ceil();
    used as f64 >= threshold
}

/// 「そろそろ畳め」と伝えるしきい値（残りステップ）。
///
/// サブエージェントの既定は 8 ステップ。実測（#407 の起票元 run）では 7 体中 6 体が
/// `Budget(Steps)` で**強制的に**切られており、そのうち何体かは findings を返せずに終わった。
/// 「残り 1〜2 ステップ」を明示できれば、最後の 1 手を「まとめて返す」に使える。
const LANDING_STEPS: usize = 2;

/// トークン/時間の残りがこの割合を切ったら着地させる。
const LANDING_FRACTION: f64 = 0.15;

/// 残り予算（**モデルへ見せる**観測）。
///
/// `None` の軸は「上限なし」。数値は次のステップに入る前の値で、ステップごとに更新される。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Remaining {
    pub steps: usize,
    /// 残りトークン（`fresh_tokens` 基準＝上限判定と同じ軸）。
    pub tokens: Option<u64>,
    /// 残り秒（`deadline` があるときだけ）。
    pub seconds: Option<u64>,
    /// 上限が近い（＝新しい調査を始めず畳むべき）か。
    pub landing: bool,
}

impl Remaining {
    /// モデルへ渡す 1 行の観測。
    ///
    /// **なぜ観測として渡すか**: `BudgetWarning` は sink（イベントログ／UI）にしか流れず、
    /// エージェント自身は自分があと何ステップ使えるかを知らないまま上限に当たって切られていた。
    /// 研究側（BAGEN 2026）でも「フロンティアモデルは自分のリソース消費を予測できない・楽観
    /// バイアスがある」と報告されており、**残量を明示して初めて畳む判断ができる**。
    /// system プロンプトへ埋めないのは、静的だと更新されないうえプロンプトキャッシュの
    /// 前置きを毎ステップ壊すため（観測なら新規ぶんに乗る）。
    #[must_use]
    pub fn observation(&self) -> String {
        let mut parts = vec![format!("残り {} ステップ", self.steps)];
        if let Some(tokens) = self.tokens {
            parts.push(format!("トークン 約 {}k", tokens / 1000));
        }
        if let Some(seconds) = self.seconds {
            parts.push(format!("時間 約 {} 分", seconds / 60));
        }
        let head = format!("[予算] {}", parts.join(" / "));
        if self.landing {
            format!(
                "{head}\n**新しい調査を始めないこと。**いま手元にある材料だけで結論を書き、\
                 この次の応答で終えること（上限に達すると途中で強制終了し、成果物は残らない）。"
            )
        } else {
            head
        }
    }
}

/// 予算判定の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetCheck {
    /// 余裕あり。
    Ok,
    /// 上限接近（種別・現在値・上限）。続行可・警告イベントを 1 回出す。
    Warn(BudgetKind, u64, u64),
    /// 超過（安全停止すべき）。
    Exceeded(BudgetKind),
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::time::Duration;

    /// 上限判定は新規ぶん（`fresh_tokens`）で行うため、テストの `tokens` はそこへ入れる
    /// （課金累計は判定に効かない）。
    fn spent(steps: usize, tokens: u64, cost: i64) -> Spent {
        Spent {
            steps,
            tokens,
            fresh_tokens: tokens,
            prev_prompt_tokens: 0,
            cost_usd_micros: cost,
        }
    }

    /// 履歴の再送は**仕事量として二重計上しない**（#404）。
    ///
    /// prompt が 1000 → 3000 → 6000 と伸びる 3 ステップで、課金累計は 10800 になるが、
    /// 新規ぶんは「増えた prompt ＋ completion」＝ 6600 に留まる。上限をこの値で見ないと、
    /// 「20 万トークン」が実際には数ステップぶんの仕事しか許さない値になる。
    #[test]
    fn fresh_tokens_count_only_new_content() {
        let mut s = Spent::default();
        s.add_step(1000, 200, 0);
        s.add_step(3000, 200, 0);
        s.add_step(6000, 200, 0);
        assert_eq!(s.tokens, 1200 + 3200 + 6200, "課金は毎回の prompt を含む");
        assert_eq!(
            s.fresh_tokens,
            6000 + 600,
            "新規は最終 prompt ＋ completion 合計"
        );
        // 剪定で prompt が縮んでも新規ぶんは減らない（負にしない）。
        s.add_step(2000, 100, 0);
        assert_eq!(s.fresh_tokens, 6700);
    }

    /// 残量はモデルへ見せる値。**判定と同じ軸**（steps / fresh_tokens）で数える（#407）。
    #[test]
    fn remaining_matches_the_axes_used_for_stopping() {
        let b = Budget::autonomous(8, None, 120_000, 600_000);
        let r = b.remaining(&spent(3, 20_000, 0), Instant::now());
        assert_eq!(r.steps, 5);
        assert_eq!(r.tokens, Some(100_000));
        assert!(!r.landing);
        let text = r.observation();
        assert!(text.contains("残り 5 ステップ"), "{text}");
        assert!(text.contains("100k"), "{text}");
        assert!(!text.contains("新しい調査"), "{text}");
    }

    /// 上限が近いと**着地指示**が出る（残りステップ・トークンのどちらでも）。
    ///
    /// 起票元の実測では委譲 7 体中 6 体が `Budget(Steps)` で切られ、findings を返せずに
    /// 終わった体もあった。残量が見えていれば最後の 1 手を「まとめて返す」に使える。
    #[test]
    fn landing_is_announced_before_the_cap() {
        let b = Budget::autonomous(8, None, 120_000, 600_000);
        let by_steps = b.remaining(&spent(6, 0, 0), Instant::now());
        assert!(by_steps.landing);
        assert!(
            by_steps.observation().contains("新しい調査を始めないこと"),
            "{}",
            by_steps.observation()
        );
        // ステップに余裕があってもトークンが残り 15% を切れば着地させる。
        let by_tokens = b.remaining(&spent(1, 110_000, 0), Instant::now());
        assert!(by_tokens.landing, "{by_tokens:?}");
    }

    /// 上限の無い軸は表示しない（`None` を「0」と誤読させない）。
    #[test]
    fn unlimited_axes_are_omitted() {
        let r = Budget::chat(8).remaining(&spent(1, 0, 0), Instant::now());
        assert_eq!(r.tokens, None);
        assert_eq!(r.seconds, None);
        assert_eq!(r.observation(), "[予算] 残り 7 ステップ");
    }

    #[test]
    fn steps_exceeded_stops() {
        let b = Budget::chat(8);
        assert_eq!(
            b.check(&spent(8, 0, 0), Instant::now()),
            BudgetCheck::Exceeded(BudgetKind::Steps)
        );
    }

    #[test]
    fn tokens_and_cost_exceeded() {
        let b = Budget::autonomous(1000, None, 5000, 10_000);
        assert_eq!(
            b.check(&spent(1, 5000, 0), Instant::now()),
            BudgetCheck::Exceeded(BudgetKind::Tokens)
        );
        assert_eq!(
            b.check(&spent(1, 0, 10_000), Instant::now()),
            BudgetCheck::Exceeded(BudgetKind::Cost)
        );
    }

    #[test]
    fn deadline_exceeded() {
        let now = Instant::now();
        let past = now - Duration::from_secs(1);
        let b = Budget {
            deadline: Some(past),
            ..Budget::chat(100)
        };
        assert_eq!(
            b.check(&spent(0, 0, 0), now),
            BudgetCheck::Exceeded(BudgetKind::Time)
        );
    }

    #[test]
    fn warns_before_limit() {
        let b = Budget::autonomous(1000, None, 100, 1_000_000);
        // 80% で警告。
        assert_eq!(
            b.check(&spent(1, 80, 0), Instant::now()),
            BudgetCheck::Warn(BudgetKind::Tokens, 80, 100)
        );
        // 79% は Ok（steps/cost も未達）。
        assert_eq!(b.check(&spent(1, 79, 0), Instant::now()), BudgetCheck::Ok);
    }

    #[test]
    fn warns_before_time_deadline() {
        let start = Instant::now();
        // 100 秒 deadline。経過 80%（80 秒）で Time 警告、79 秒は Ok。
        let b = Budget {
            started_at: Some(start),
            deadline: Some(start + Duration::from_secs(100)),
            ..Budget::chat(1000)
        };
        assert_eq!(
            b.check(&spent(1, 0, 0), start + Duration::from_secs(80)),
            BudgetCheck::Warn(BudgetKind::Time, 80, 100)
        );
        assert_eq!(
            b.check(&spent(1, 0, 0), start + Duration::from_secs(79)),
            BudgetCheck::Ok
        );
    }

    fn extension() -> BudgetExtension {
        BudgetExtension {
            on_tools: vec![ToolName::EmitWorkflow],
            max_steps: 24,
            max_fresh_tokens: 600_000,
            max_cost_usd_micros: 2_000_000,
            max_response_tokens: 32_768,
        }
    }

    fn assistant_calls(name: &str) -> Message {
        Message {
            role: llm_gateway::Role::Assistant,
            content: vec![Block::ToolUse {
                id: "c1".into(),
                name: name.into(),
                input: serde_json::json!({}),
            }],
        }
    }

    fn tool_result(content: &str, is_error: bool, loaded: &[&str]) -> Message {
        Message {
            role: llm_gateway::Role::Tool,
            content: vec![Block::ToolResult {
                tool_use_id: "c1".into(),
                content: content.into(),
                is_error,
                tool_references: loaded.iter().map(|s| (*s).to_string()).collect(),
            }],
        }
    }

    /// 呼び出しがあれば広げる。失敗した呼び出しも数え、`tool_search` で読み込んだだけでは広げない（D-204）。
    #[test]
    fn extension_is_derived_from_calls_in_the_history() {
        let ext = extension();
        let user = Message::text(llm_gateway::Role::User, "経費のワークフローを作って");
        assert!(!ext.called(std::slice::from_ref(&user)));
        // tool_search が emit_workflow の定義を読み込んだだけ（呼び出しは tool_search のもの）。
        let loaded = [
            user.clone(),
            assistant_calls("tool_search"),
            tool_result("emit_workflow を読み込みました", false, &["emit_workflow"]),
        ];
        assert!(!ext.called(&loaded), "読み込んだだけでは広げない");
        // 本文に名前が出るだけでも広げない（呼び出しではない）。
        let named = [
            user.clone(),
            Message::text(llm_gateway::Role::Assistant, "emit_workflow で作ります"),
        ];
        assert!(!ext.called(&named));
        // 検証で断られた呼び出しも「呼んだ」に数える。
        let failed = [
            user.clone(),
            assistant_calls("emit_workflow"),
            tool_result("ir.unknown_node", true, &[]),
        ];
        assert!(ext.called(&failed));
        // 関係のない道具では広げない。
        let other = [user, assistant_calls("doc_search")];
        assert!(!ext.called(&other));
    }

    /// 広げた予算は固定値まで。元より小さい値には下げず、上限の無い軸は無いまま。時間の上限は変えない。
    #[test]
    fn extension_raises_only_steps_tokens_cost_and_response() {
        let ext = extension();
        let deadline = Instant::now() + Duration::from_secs(60);
        let base = Budget {
            deadline: Some(deadline),
            max_tokens: Some(300_000),
            max_cost_usd_micros: Some(1_000_000),
            ..Budget::chat(6)
        };
        let wide = ext.extend(&base);
        assert_eq!(wide.max_steps, 24);
        assert_eq!(wide.max_tokens, Some(600_000));
        assert_eq!(wide.max_cost_usd_micros, Some(2_000_000));
        assert_eq!(wide.deadline, Some(deadline), "時間の上限は変えない");
        assert!((wide.warn_at_fraction - base.warn_at_fraction).abs() < f64::EPSILON);
        // 広げた後にもう一度広げても同じ（何度呼んでも 1 回ぶん）。
        assert_eq!(ext.extend(&wide).max_steps, 24);
        assert_eq!(ext.extend(&wide).max_tokens, Some(600_000));
        // 元の方が大きい軸は下げない。上限の無い軸は無いまま。
        let big = Budget {
            max_tokens: None,
            max_cost_usd_micros: Some(8_000_000),
            ..Budget::chat(50)
        };
        let kept = ext.extend(&big);
        assert_eq!(kept.max_steps, 50);
        assert_eq!(kept.max_tokens, None);
        assert_eq!(kept.max_cost_usd_micros, Some(8_000_000));
        // 1 応答の出力上限。
        assert_eq!(ext.extend_response_tokens(Some(8192)), Some(32_768));
        assert_eq!(ext.extend_response_tokens(Some(65_536)), Some(65_536));
        assert_eq!(ext.extend_response_tokens(None), Some(32_768));
    }

    /// 6 手で止まる会話も、広げた予算では続く（判定は広げた値で行う）。
    #[test]
    fn extended_budget_lets_the_run_continue_past_the_base_steps() {
        let base = Budget::chat(6);
        assert_eq!(
            base.check(&spent(6, 0, 0), Instant::now()),
            BudgetCheck::Exceeded(BudgetKind::Steps)
        );
        let wide = extension().extend(&base);
        assert_eq!(wide.check(&spent(6, 0, 0), Instant::now()), BudgetCheck::Ok);
        assert_eq!(
            wide.check(&spent(24, 0, 0), Instant::now()),
            BudgetCheck::Exceeded(BudgetKind::Steps)
        );
    }

    #[test]
    fn unlimited_axis_never_triggers() {
        let b = Budget::chat(1000); // token/cost は None
        assert_eq!(
            b.check(&spent(1, u64::MAX, i64::MAX), Instant::now()),
            BudgetCheck::Ok
        );
    }

    proptest! {
        // 超過は必ず安全停止（Exceeded）を返す。決して Ok にならない。
        #[test]
        fn exceeded_never_ok(steps in 0usize..50, tokens in 0u64..10_000, cost in 0i64..10_000) {
            let b = Budget::autonomous(10, None, 1000, 500);
            let s = spent(steps, tokens, cost);
            let over = steps >= 10 || tokens >= 1000 || cost >= 500;
            let res = b.check(&s, Instant::now());
            if over {
                prop_assert!(matches!(res, BudgetCheck::Exceeded(_)));
            }
        }

        // check は now を進めても steps/token/cost 判定を壊さない（時間軸のみ now 依存）。
        #[test]
        fn monotonic_in_tokens(t1 in 0u64..1000, t2 in 0u64..1000) {
            let b = Budget::autonomous(1_000_000, None, 1000, 1_000_000);
            let (lo, hi) = (t1.min(t2), t1.max(t2));
            let r_lo = b.check(&spent(1, lo, 0), Instant::now());
            let r_hi = b.check(&spent(1, hi, 0), Instant::now());
            // 消費が多い方が「より逼迫」: Ok<Warn<Exceeded の順序を崩さない。
            let rank = |c: BudgetCheck| match c {
                BudgetCheck::Ok => 0,
                BudgetCheck::Warn(..) => 1,
                BudgetCheck::Exceeded(_) => 2,
            };
            prop_assert!(rank(r_lo) <= rank(r_hi));
        }
    }
}
