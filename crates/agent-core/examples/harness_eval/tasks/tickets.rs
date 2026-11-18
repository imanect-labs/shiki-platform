//! チケット処理課題: 規程を読み、10 件の問い合わせを顧客情報と突き合わせて処理する。
//!
//! 1 件ごとに「チケット取得 → 顧客照会 → 規程で判断 → 処理」と手数が要る長ホライズン課題。
//! 顧客照会の一部は初回だけ一時障害を返す（再試行できるか）。境界値（29 日・31 日）と、
//! 規程の優先順位（重複 > データ損失、返金回数 > 期間）で判断の正確さを見る。
//! 処理済みのチケットを再処理すると拒否される（二重処理を数える）。

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use agent_core::Tool;
use serde_json::json;

use super::{arg_str, FnTool, Grade, Task};

const POLICY: &str = "# サポート対応規程（2026 年 11 月版）\n\n\
本日は 2026-11-18 とする。各チケットは resolve_ticket で 1 回だけ処理する。action は次のいずれか: \
refund / escalate / reply / close_duplicate。\n\n\
## 判断の優先順位（上から順に当てはめ、最初に該当したものを採る）\n\n\
1. **重複**: 本文が既存チケットと同じ事象だと明記している（例「T-1234 と同じ」）→ close_duplicate。\
   参照先の処理状況は問わない。\n\
2. **返金依頼**:\n\
   - 顧客の直近 12 か月の返金回数（refunds_last_12m）が 2 回以上 → escalate（不正防止のため審査部門へ）。\n\
   - そうでなく、プランが premium で、購入日から本日まで 30 日以内（30 日目を含む）→ refund。\n\
   - それ以外 → reply（返金できない旨を案内）。\n\
3. **不具合報告**: データの消失・破損を伴う → escalate。それ以外 → reply。\n\n\
## 注意\n\n\
- 購入日・プラン・返金回数は必ず get_customer で確認する。チケット本文の自己申告は使わない。\n\
- note には判断の根拠を一文で書く。\n\
- 顧客システムは時々一時的に応答しない。エラーが出たら同じ照会をやり直す。\n";

#[derive(Clone)]
struct TicketDef {
    id: String,
    customer: String,
    subject: String,
    body: String,
    expected: String,
}

#[derive(Clone)]
struct CustomerDef {
    id: String,
    plan: String,
    purchase: String,
    refunds: u32,
}

/// 規定の 10 件（境界値・優先順位の罠を手で置いたもの）。
fn fixed_data() -> (Vec<TicketDef>, Vec<CustomerDef>, Vec<String>) {
    let t = |id: &str, c: &str, subject: &str, body: &str, expected: &str| TicketDef {
        id: id.into(),
        customer: c.into(),
        subject: subject.into(),
        body: body.into(),
        expected: expected.into(),
    };
    let tickets = vec![
        t(
            "T-1001",
            "C-01",
            "返金のお願い",
            "先日購入したプランですが、思っていた機能と違ったので返金をお願いします。",
            "refund",
        ),
        t(
            "T-1002",
            "C-02",
            "返金について",
            "購入して一週間ですが、使わなくなったので返金してほしいです。私はプレミアム会員です。",
            "reply",
        ),
        t(
            "T-1003",
            "C-03",
            "保存したデータが消えた",
            "昨日保存したはずのプロジェクトのデータが、今朝開いたら全部消えていました。",
            "escalate",
        ),
        t(
            "T-1004",
            "C-04",
            "ボタンの色",
            "設定画面の保存ボタンの色が薄くて見えにくいです。",
            "reply",
        ),
        t(
            "T-1005",
            "C-05",
            "返金希望",
            "あまり使わなかったので返金をお願いできますか。",
            "reply",
        ),
        t(
            "T-1006",
            "C-06",
            "データ消失",
            "T-1003 と同じ現象です。保存したデータが消えています。",
            "close_duplicate",
        ),
        t(
            "T-1007",
            "C-07",
            "返金してください",
            "購入したばかりですが不要になりました。返金をお願いします。",
            "escalate",
        ),
        t(
            "T-1008",
            "C-08",
            "ログインが遅い",
            "朝の時間帯にログインが 10 秒ほどかかります。",
            "reply",
        ),
        t(
            "T-1009",
            "C-09",
            "返金の相談",
            "期待した使い方ができなかったので、返金を検討しています。",
            "refund",
        ),
        t(
            "T-1010",
            "C-10",
            "返金依頼",
            "購入から 3 週間ほどですが、返金をお願いします。",
            "reply",
        ),
    ];
    let c = |id: &str, plan: &str, purchase: &str, refunds: u32| CustomerDef {
        id: id.into(),
        plan: plan.into(),
        purchase: purchase.into(),
        refunds,
    };
    let customers = vec![
        c("C-01", "premium", "2026-11-02", 0),
        c("C-02", "standard", "2026-11-10", 0),
        c("C-03", "premium", "2026-03-01", 0),
        c("C-04", "standard", "2025-12-12", 1),
        c("C-05", "premium", "2026-09-01", 0),
        c("C-06", "standard", "2026-06-30", 0),
        c("C-07", "premium", "2026-11-15", 2),
        c("C-08", "premium", "2026-01-20", 0),
        c("C-09", "premium", "2026-10-19", 1),
        c("C-10", "premium", "2026-10-17", 0),
    ];
    (tickets, customers, vec!["C-05".into(), "C-09".into()])
}

/// 規程どおりの正解（生成データの採点に使う）。
fn policy_action(
    kind: &str,
    dup_of: Option<&str>,
    data_loss: bool,
    c: &CustomerDef,
) -> &'static str {
    if dup_of.is_some() {
        return "close_duplicate";
    }
    match kind {
        "refund" => {
            let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 18).expect("date");
            let bought = chrono::NaiveDate::parse_from_str(&c.purchase, "%Y-%m-%d").expect("date");
            let days = (today - bought).num_days();
            if c.refunds >= 2 {
                "escalate"
            } else if c.plan == "premium" && days <= 30 {
                "refund"
            } else {
                "reply"
            }
        }
        _ if data_loss => "escalate",
        _ => "reply",
    }
}

/// 長い版: 30 件を擬似乱数で作る（正解は規程の関数で決める）。
fn long_data(seed: u64) -> (Vec<TicketDef>, Vec<CustomerDef>, Vec<String>) {
    let mut rng = super::Rng::new(seed ^ 0x5eed);
    let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 18).expect("date");
    let mut tickets = Vec::new();
    let mut customers = Vec::new();
    let mut flaky = Vec::new();
    let refund_bodies = [
        "思っていた機能と違ったので返金をお願いします。",
        "使わなくなったので返金してほしいです。",
        "購入から日が浅いので返金できますか。プレミアムです。",
        "期待した使い方ができませんでした。返金を希望します。",
    ];
    let bug_ok = [
        "画面の文字が小さくて読みにくいです。",
        "通知が 2 回届くことがあります。",
        "検索結果の並び順が分かりにくいです。",
    ];
    let bug_loss = [
        "保存したファイルが消えていました。",
        "同期の後、データが破損して開けません。",
    ];
    for i in 0..30u32 {
        let id = format!("T-2{i:03}");
        let cid = format!("C-2{i:02}");
        let plan = if rng.range(0, 2) == 0 {
            "standard"
        } else {
            "premium"
        };
        // 境界付近（28〜33 日）を多めに混ぜる。
        let days = if rng.range(0, 2) == 0 {
            rng.range(28, 33)
        } else {
            rng.range(1, 120)
        };
        let purchase = (today - chrono::Duration::days(days as i64))
            .format("%Y-%m-%d")
            .to_string();
        let refunds = [0, 0, 0, 1, 2, 3][rng.range(0, 5) as usize];
        let c = CustomerDef {
            id: cid.clone(),
            plan: plan.into(),
            purchase,
            refunds,
        };
        let roll = rng.range(0, 9);
        let (kind, subject, body, dup, loss) = if i > 3 && roll == 0 {
            let target = format!("T-2{:03}", rng.range(0, u64::from(i) - 1));
            (
                "bug",
                "前と同じ件".to_string(),
                format!("{target} と同じ現象です。"),
                Some(target),
                false,
            )
        } else if roll <= 4 {
            (
                "refund",
                "返金依頼".to_string(),
                (*rng.pick(&refund_bodies)).to_string(),
                None,
                false,
            )
        } else if roll <= 6 {
            (
                "bug",
                "不具合".to_string(),
                (*rng.pick(&bug_loss)).to_string(),
                None,
                true,
            )
        } else {
            (
                "bug",
                "不具合".to_string(),
                (*rng.pick(&bug_ok)).to_string(),
                None,
                false,
            )
        };
        let expected = policy_action(kind, dup.as_deref(), loss, &c).to_string();
        if rng.range(0, 6) == 0 {
            flaky.push(cid.clone());
        }
        tickets.push(TicketDef {
            id,
            customer: cid,
            subject,
            body,
            expected,
        });
        customers.push(c);
    }
    (tickets, customers, flaky)
}

/// 本文の後ろに付く、やり取りの履歴（文脈を埋める）。
fn thread_history(id: &str, n: usize) -> String {
    let mut s = String::from("\n\n--- やり取りの履歴 ---\n");
    for i in 1..=n {
        let _ = write!(
            s,
            "[{id}-{i}] 自動応答: お問い合わせありがとうございます。担当者が内容を確認しております。\
             通常 2 営業日以内にご連絡いたします。よくあるご質問はヘルプセンターをご覧ください。\
             なお、混雑状況により回答までお時間をいただく場合がございます。ご了承ください。\n"
        );
    }
    s
}

#[derive(Default)]
struct State {
    resolved: BTreeMap<String, (String, String)>,
    duplicate_attempts: usize,
    flaky_seen: HashSet<String>,
}

pub(crate) struct Tickets {
    state: Arc<Mutex<State>>,
    tickets: Arc<Vec<TicketDef>>,
    customers: Arc<Vec<CustomerDef>>,
    flaky: Arc<Vec<String>>,
    long: bool,
}

impl Tickets {
    pub(crate) fn new() -> Self {
        Self::from_data(fixed_data(), false)
    }

    /// 30 件の長い版（履歴も長く、32k 窓では圧縮が要る）。
    pub(crate) fn long(seed: u64) -> Self {
        Self::from_data(long_data(seed), true)
    }

    fn from_data(data: (Vec<TicketDef>, Vec<CustomerDef>, Vec<String>), long: bool) -> Self {
        Tickets {
            state: Arc::new(Mutex::new(State::default())),
            tickets: Arc::new(data.0),
            customers: Arc::new(data.1),
            flaky: Arc::new(data.2),
            long,
        }
    }
}

impl Task for Tickets {
    fn system(&self) -> String {
        "あなたはカスタマーサポート部門の処理担当アシスタントです。".into()
    }

    fn prompt(&self) -> String {
        "未処理のサポートチケットがたまっています。read_policy で対応規程を読み、規程どおりに\
         未処理チケットをすべて処理してください。終わったら、各チケットの処理結果を一覧で報告してください。"
            .into()
    }

    fn max_steps(&self) -> usize {
        if self.long {
            70
        } else {
            40
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let (s1, s2, s3, s4) = (
            Arc::clone(&self.state),
            Arc::clone(&self.state),
            Arc::clone(&self.state),
            Arc::clone(&self.state),
        );
        let (t1, t2, t3, t4) = (
            Arc::clone(&self.tickets),
            Arc::clone(&self.tickets),
            Arc::clone(&self.tickets),
            Arc::clone(&self.tickets),
        );
        let customers = Arc::clone(&self.customers);
        let flaky = Arc::clone(&self.flaky);
        let history = if self.long { 12 } else { 6 };
        vec![
            FnTool::new(
                "read_policy",
                "サポート対応規程の全文を返す。",
                json!({"type": "object", "properties": {}}),
                true,
                |_| Ok(POLICY.to_string()),
            ),
            FnTool::new(
                "list_tickets",
                "未処理チケットの ID と件名の一覧を返す。",
                json!({"type": "object", "properties": {}}),
                true,
                move |_| {
                    let st = s1.lock().unwrap();
                    let open: Vec<String> = t1
                        .iter()
                        .filter(|t| !st.resolved.contains_key(&t.id))
                        .map(|t| format!("{} {}", t.id, t.subject))
                        .collect();
                    Ok(if open.is_empty() { "未処理チケットはありません".into() } else { open.join("\n") })
                },
            ),
            FnTool::new(
                "get_ticket",
                "チケット 1 件の本文と顧客 ID を返す。",
                json!({"type": "object", "properties": {"id": {"type": "string", "description": "例: T-1001"}}, "required": ["id"]}),
                true,
                move |v| {
                    let id = arg_str(v, "id")?;
                    let t = t2.iter().find(|t| t.id == id).ok_or_else(|| format!("チケット {id} は存在しません"))?;
                    Ok(format!(
                        "ID: {}\n件名: {}\n顧客 ID: {}\n本文: {}{}",
                        t.id, t.subject, t.customer, t.body, thread_history(&t.id, history)
                    ))
                },
            ),
            FnTool::new(
                "get_customer",
                "顧客情報（プラン・購入日・直近 12 か月の返金回数）を返す。",
                json!({"type": "object", "properties": {"customer_id": {"type": "string", "description": "例: C-01"}}, "required": ["customer_id"]}),
                true,
                move |v| {
                    let id = arg_str(v, "customer_id")?;
                    let c = customers.iter().find(|c| c.id == id).ok_or_else(|| format!("顧客 {id} は存在しません"))?;
                    {
                        let mut st = s2.lock().unwrap();
                        if flaky.iter().any(|f| f == id) && st.flaky_seen.insert(id.to_string()) {
                            return Err("503 Service Unavailable: 顧客システムが一時的に応答しません。しばらくしてから再試行してください。".into());
                        }
                    }
                    Ok(format!(
                        "顧客 ID: {}\nプラン: {}\n購入日: {}\n直近 12 か月の返金回数: {}\n住所: 東京都千代田区（省略）\n\
                         連絡先: 登録済み\n備考: 特になし",
                        c.id, c.plan, c.purchase, c.refunds
                    ))
                },
            ),
            FnTool::new(
                "resolve_ticket",
                "チケットを処理する（1 件 1 回）。action は refund / escalate / reply / close_duplicate。",
                json!({"type": "object", "properties": {
                    "id": {"type": "string"},
                    "action": {"type": "string", "enum": ["refund", "escalate", "reply", "close_duplicate"]},
                    "note": {"type": "string"}
                }, "required": ["id", "action", "note"]}),
                false,
                move |v| {
                    let id = arg_str(v, "id")?;
                    let action = arg_str(v, "action")?;
                    let note = arg_str(v, "note")?;
                    if !["refund", "escalate", "reply", "close_duplicate"].contains(&action) {
                        return Err(format!("invalid action: {action}"));
                    }
                    if !t3.iter().any(|t| t.id == id) {
                        return Err(format!("チケット {id} は存在しません"));
                    }
                    let mut st = s3.lock().unwrap();
                    if st.resolved.contains_key(id) {
                        st.duplicate_attempts += 1;
                        return Err(format!("チケット {id} は処理済みです"));
                    }
                    st.resolved.insert(id.into(), (action.into(), note.into()));
                    Ok(format!("{id} を {action} で処理しました"))
                },
            ),
            FnTool::new(
                "count_open",
                "未処理チケットの件数を返す。",
                json!({"type": "object", "properties": {}}),
                true,
                move |_| {
                    let st = s4.lock().unwrap();
                    Ok(format!("未処理 {} 件", t4.len() - st.resolved.len()))
                },
            ),
        ]
    }

    fn grade(&self) -> Grade {
        let st = self.state.lock().unwrap();
        let mut correct = 0;
        let mut wrong = Vec::new();
        for t in self.tickets.iter() {
            match st.resolved.get(&t.id) {
                Some((a, _)) if *a == t.expected => correct += 1,
                Some((a, _)) => wrong.push(json!({"id": t.id, "got": a, "expected": t.expected})),
                None => wrong.push(json!({"id": t.id, "got": null, "expected": t.expected})),
            }
        }
        let score = f64::from(correct) / self.tickets.len() as f64;
        Grade {
            score,
            success: correct as usize == self.tickets.len(),
            details: json!({"wrong": wrong, "duplicate_attempts": st.duplicate_attempts}),
        }
    }
}
