//! 共有の相手がいる会話の試運転の観測（PIT-84 の (b)・D-214）。
//!
//! 会話に話した人のほかの相手がいるとき（[`crate::store::ChatStore`] の `thread_audience`）は、全部入りの観測
//! （[`super::observe`]）の代わりにこの形を返す。返すのは状態・件数・画面の道と、固定の案内だけにする。
//!
//! - **返す**: 経路の印と状態の語・run の id・版・workflow_id（画面の道の部品・モデル自身が渡した値）・step と
//!   レコードの総数・画面の道・固定の案内。
//! - **外す**: IR と run の中身の全部（表示名・参照名・ブロックの id とラベルと種類・step ごとの状態・出口・誤りの code
//!   と文・出力の先頭・通らなかった理由・繰り返しの内訳）、失敗の分類と参照番号、作ったレコードの table_id と id、
//!   費用の数字、step の状態ごとの内訳。
//! - IR や run の値で出し分けていた注記（スケジュール・できごとのきっかけと、費用）は、出し分けそのものが 1 ビットを
//!   漏らすので、一般の文にして毎回出す。
//! - 封筒（`<run_output>`）も注意書きも付けない。作成者や外の相手が書いた文字が 1 つも入らないため。
//!
//! 入力は許す値だけを持つ [`SharedFacts`] にする。[`super::observe::Observed`] を読むのは
//! [`SharedFacts::from_observed`] の 1 か所だけで、文を組む関数は `WorkflowIr` も `RunDigest` も受け取らない。後で
//! 全部入りの側に欄を足しても、型の上で共有の側には出ない。
//!
//! この観測はそのまま tool_result として保存されるので、本人の画面・相手の画面・次の回答の文脈・分岐・書き出しの
//! どれにも、この形だけが届く。全部入りの中身は run の画面で確かめてもらう。

use std::fmt::Write as _;
use std::time::Duration;

use uuid::Uuid;
use workflow_engine::{RunStatus, WaitingFor};

use super::observe::{self, Observed, Origin};
use super::wait::WaitEnd;

/// 中身を返していないことと、どこで確かめられるか。
pub(super) const WITHHELD_NOTE: &str =
    "この会話には共有の相手がいるので、実行の中身（ブロックごとの結果・出力・誤り・作ったレコード・費用）は返していま\
     せん。中身は run の画面で確かめられます（開けるのは、このワークフローを見られる人です）。";

/// 失敗したときの案内（どのブロックで何が起きたかは返さない）。
pub(super) const FAILED_SHARED: &str =
    "失敗しました。どのブロックで何が起きたかは、共有の相手がいる会話には返しません。ユーザーに run の画面で確かめて\
     もらい、どこを直すかを聞いてから、emit_workflow で保存した新しい版で試し直してください。";

/// スケジュール・できごとのきっかけの注記（IR に依らず毎回出す）。
pub(super) const SCHEDULE_NOTE_GENERAL: &str =
    "スケジュールやできごとをきっかけに動くワークフローなら、本番は有効にしたときの委譲（ワークフローの権限）で動くので、\
     あなたの権限で動かした試運転と結果が違うことがあります。";

/// 費用の注記（費用の有無に依らず毎回出す）。
pub(super) const COST_NOTE_GENERAL: &str =
    "試運転で使った LLM の費用は、この会話の予算に積みます（重いワークフローでは、予算の上限で止まることがあります）。";

/// 名前で呼ばれた（名前を解決しないので、id も最新の版も言わない）。
const NAMED: &str =
    "起動していません。workflow_id には名前ではなく id（UUID）を入れてください。list_workflows で id と版を確かめて\
     呼び直してください（名前では起動しません）。";

/// 起動されなかった（同時に動かせる数の上限・設定の名前は出さない）。
pub(super) const NOT_LAUNCHED: &str =
    "起動されませんでした（同時に動かせる数の上限）。前の実行が終わってから試してください。";

/// 入力の誤りで起動できなかったときの案内（どの項目かは返さない）。
pub(super) const INPUT_REFUSED_GUIDANCE: &str =
    "どの項目かは、共有の相手がいる会話には返しません。input を見直して呼び直すか、ユーザーにワークフローの画面で\
     確かめてもらってください。";

/// 実行の前に止める設定で起動できなかったときの案内（どの欄かは返さない）。
pub(super) const NOT_READY_GUIDANCE: &str =
    "どの欄かは、共有の相手がいる会話には返しません。ユーザーにワークフローの画面で空の欄を確かめてもらい、\
     emit_workflow で保存した新しい版で試してください。";

/// 共有の相手がいる会話の観測の材料（許す値だけ）。
pub(super) struct SharedFacts {
    pub origin: Origin,
    /// run の状態（要約が読めなければ `None`）。
    pub state: Option<(RunStatus, Option<WaitingFor>)>,
    pub run_id: Uuid,
    pub version: i64,
    /// 画面の道の部品（モデル自身が渡した値）。
    pub workflow_id: Uuid,
    /// step の総数と、作った・書き換えたレコードの総数（要約が読めなければ `None`）。
    pub counts: Option<(usize, i64)>,
    /// 待つのをやめた理由（前回の結果は `None`）。
    pub end: Option<WaitEnd>,
    pub max_wait: Duration,
}

impl SharedFacts {
    /// 全部入りの材料から、許す値だけを取り出す（[`Observed`] を読むのはここだけ）。
    pub(super) fn from_observed(o: &Observed<'_>) -> Self {
        SharedFacts {
            origin: o.origin,
            state: observe::run_state(o.digest),
            run_id: o.run_id,
            version: o.version,
            workflow_id: o.workflow_id,
            counts: observe::run_counts(o.digest),
            end: o.end,
            max_wait: o.max_wait,
        }
    }
}

/// 観測の本文（先頭の 1 行・画面の道・断り・状態の案内・添え書き・やり直し方・片付け・注記の順）。
pub(super) fn render(f: &SharedFacts) -> String {
    let mut out = observe::head_core(f.origin, f.state, f.run_id, f.version, f.counts);
    let _ = write!(
        out,
        "\n画面: /workflows/{}/runs/{}",
        f.workflow_id, f.run_id
    );
    let guidance = observe::state_guidance(f.state, f.end, f.max_wait, FAILED_SHARED);
    for line in [WITHHELD_NOTE, guidance.as_str()] {
        out.push('\n');
        out.push_str(line);
    }
    if let Some(note) = observe::origin_note(f.origin) {
        out.push('\n');
        out.push_str(note);
    }
    out.push('\n');
    out.push_str(observe::RETRY_NOTE);
    if let Some((_, records)) = f.counts.filter(|&(_, records)| records > 0) {
        let _ = write!(
            out,
            "\n試運転で作った・書き換えたレコード（{records} 件）は本物のテーブルに残ります。run の画面で確かめ、テーブルの\
             画面で該当の行を開いて消す手順を、最後の回答に書いてください。"
        );
    }
    for line in [SCHEDULE_NOTE_GENERAL, COST_NOTE_GENERAL] {
        out.push('\n');
        out.push_str(line);
    }
    out
}

/// 名前で呼ばれたときの文（名前を解決しない）。
pub(super) fn named() -> String {
    NAMED.to_string()
}

/// 渡した版が無いときの文（今の最新の版は言わない）。
pub(super) fn no_such_version(version: i64) -> String {
    format!("v{version} はありません。list_workflows で版を確かめて呼び直してください。")
}

/// 起動できなかったときの文（理由の文は返さず、件数だけ）。
pub(super) fn launch_refused(
    workflow_id: Uuid,
    version: i64,
    what: &str,
    count: usize,
    guidance: &str,
) -> String {
    format!(
        "試運転: 起動できませんでした（{what} {count} 件） / 版: v{version} / workflow_id: {workflow_id}\n{guidance}"
    )
}

#[cfg(test)]
#[path = "shared_tests.rs"]
mod tests;
