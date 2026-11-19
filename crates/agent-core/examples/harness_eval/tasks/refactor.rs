//! 複数ファイルのリファクタ課題: Python 風のプロジェクト（12 ファイル・約 1,500 行）で、関数
//! `calc_total(items, tax)` を `compute_total(items, tax_rate, *, rounding="half_up")` に改める。
//!
//! 罠（雑な一括置換で壊れるもの）:
//! - 似た名前の別関数 `calc_totals_by_day` / `recalc_total` は変えない。
//! - キーワード引数 `tax=` は `calc_total` の呼び出しでだけ `tax_rate=` に変える
//!   （`format_receipt(..., tax=...)` は変えない）。`order.tax` 属性も変えない。
//! - CHANGELOG.md の過去の項目は変えず、先頭に `## Unreleased` の項目を足す。
//!
//! 期待される最終形はテンプレートを新旧の名前で 2 回描いて作る（採点は完全一致）。

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use agent_core::Tool;
use serde_json::json;

use super::{vfs, FnTool, Grade, Rng, Task};

const VERBS: &[&str] = &[
    "load",
    "parse",
    "merge",
    "split",
    "format",
    "check",
    "build",
    "collect",
    "normalize",
    "sync",
    "export",
    "resolve",
];
const NOUNS: &[&str] = &[
    "rows", "lines", "orders", "users", "batch", "window", "ledger", "cache", "events", "items",
    "prices", "slots",
];
const MODULES: &[&str] = &[
    "billing/__init__.py",
    "checkout/cart.py",
    "checkout/api.py",
    "checkout/coupon.py",
    "reports/daily.py",
    "reports/monthly.py",
    "admin/tools.py",
    "scripts/backfill.py",
    "tests/test_billing.py",
    "tests/test_checkout.py",
];

/// 新旧の名前（テンプレートの差し込み先）。
struct Names {
    ct: &'static str,
    kw: &'static str,
    param: &'static str,
    sig: &'static str,
}

const OLD: Names = Names {
    ct: "calc_total",
    kw: "tax",
    param: "tax",
    sig: "def calc_total(items, tax):",
};
const NEW: Names = Names {
    ct: "compute_total",
    kw: "tax_rate",
    param: "tax_rate",
    sig: "def compute_total(items, tax_rate, *, rounding=\"half_up\"):",
};

fn render(t: &str, n: &Names) -> String {
    t.replace("{SIG}", n.sig)
        .replace("{CT}", n.ct)
        .replace("{KW}", n.kw)
        .replace("{P}", n.param)
}

/// 中身の無い補助関数（文脈を占める・決定的）。
fn filler(rng: &mut Rng, count: usize) -> String {
    let mut s = String::new();
    for _ in 0..count {
        let name = format!("{}_{}", rng.pick(VERBS), rng.pick(NOUNS));
        let arg = rng.pick(NOUNS);
        let _ = write!(
            s,
            "def {name}_{n}({arg}, limit=100):\n    \"\"\"{arg} を {limit} 件ずつ処理する補助。\"\"\"\n    out = []\n    for i, v in enumerate({arg}):\n        if i >= limit:\n            break\n        out.append(v)\n    # 空なら None を返す（呼び出し側で既定値に置き換える）\n    return out or None\n\n\n",
            n = rng.range(1, 99),
            limit = rng.range(10, 500),
        );
    }
    s
}

/// 呼び出しの断片（`{CT}` 等はテンプレートの差し込み先）。
const CALLS: &[&str] = &[
    "def total_for_cart(cart, conf):\n    # {CT} で税込みの合計を出す\n    total = {CT}(cart.items, {KW}=conf.tax)\n    log.info(\"{CT} done: %s\", total)\n    return format_receipt(total, tax=conf.tax)\n\n\n",
    "def preview(order):\n    value = {CT}(order.items, order.tax)\n    again = recalc_total(order)\n    return value, again\n\n\n",
    "def daily_summary(days, rate):\n    per_day = calc_totals_by_day(days)\n    grand = {CT}([d.item for d in days], {KW}=rate)\n    return per_day, grand\n\n\n",
    "def safe_total(items, rate):\n    try:\n        return {CT}(items, rate)\n    except ValueError:\n        log.warning(\"{CT} failed; falling back to 0\")\n        return 0\n\n\n",
];

const TEST_CALLS: &[&str] = &[
    "def test_total_with_tax():\n    items = [Item(price=100, qty=2)]\n    assert {CT}(items, {KW}=0.1) == 220\n\n\n",
    "def test_total_zero_tax():\n    assert {CT}([Item(price=50, qty=1)], 0) == 50\n\n\n",
    "def test_recalc_uses_order_tax():\n    order = Order(items=[Item(price=10, qty=1)], tax=0.5)\n    assert recalc_total(order) == 15\n\n\n",
];

const CORE: &str = "\"\"\"請求の計算（税込みの合計・日別の合計・手数料）。\"\"\"\nimport logging\n\nlog = logging.getLogger(__name__)\n\n__all__ = [\"{CT}\", \"calc_totals_by_day\", \"recalc_total\", \"apply_fee\"]\n\n\n{SIG}\n    \"\"\"品目の小計に税率 {P} を掛けて、税込みの合計を返す。\"\"\"\n    subtotal = sum(i.price * i.qty for i in items)\n    return round(subtotal * (1 + {P}))\n\n\ndef calc_totals_by_day(days):\n    \"\"\"日ごとの合計（{CT} とは別物・税は掛けない）。\"\"\"\n    return {d.date: d.amount for d in days}\n\n\ndef recalc_total(order):\n    \"\"\"注文に保存された税率で合計を計算し直す。\"\"\"\n    return {CT}(order.items, order.tax)\n\n\ndef apply_fee(total, fee=0):\n    return total + fee\n\n\ndef format_receipt(total, tax=0.0):\n    return f\"合計 {total} 円（税率 {tax}）\"\n\n\n";

const DOC: &str = "# 請求の計算\n\n## 税込みの合計\n\n`{CT}(items, {P})` は品目の小計に税率を掛けて、税込みの合計を返します。\n日ごとの合計は `calc_totals_by_day(days)` を使います（税は掛けません）。\n\n```python\nfrom billing.core import {CT}\n\ntotal = {CT}(cart.items, {KW}=0.1)\n```\n\n保存済みの注文を計算し直すときは `recalc_total(order)` を使います。\n";

const CHANGELOG_OLD: &str = "# Changelog\n\n## 1.4.0\n- calc_total が負の数量を拒否するようにした\n- calc_totals_by_day を追加\n\n## 1.3.0\n- calc_total の丸めを四捨五入に統一\n- recalc_total を追加\n";

pub(crate) struct Refactor {
    original: BTreeMap<String, String>,
    expected: BTreeMap<String, String>,
    files: vfs::Files,
    done: Arc<Mutex<Option<String>>>,
}

fn build(rng_seed: u64, n: &Names, xl: bool) -> BTreeMap<String, String> {
    let scale = if xl { 2 } else { 1 };
    let mut modules: Vec<String> = MODULES.iter().map(|m| (*m).to_string()).collect();
    if xl {
        modules.extend((1..=14).map(|k| format!("plugins/plugin_{k:02}.py")));
    }
    let mut rng = Rng::new(rng_seed);
    let mut files = BTreeMap::new();
    files.insert(
        "billing/core.py".to_string(),
        render(&format!("{CORE}{}", filler(&mut rng, 6 * scale)), n),
    );
    for (i, path) in modules.iter().enumerate() {
        let mut t = String::new();
        if path == "billing/__init__.py" {
            t.push_str("from billing.core import {CT}, calc_totals_by_day, recalc_total, apply_fee\n\n__all__ = [\"{CT}\", \"calc_totals_by_day\", \"recalc_total\", \"apply_fee\"]\n");
            files.insert(path.clone(), render(&t, n));
            continue;
        }
        let test = path.starts_with("tests/");
        let _ = write!(
            t,
            "\"\"\"{path} の処理。\"\"\"\nimport logging\n\nfrom billing.core import {{CT}}, recalc_total, calc_totals_by_day, format_receipt\n{}\nlog = logging.getLogger(__name__)\n\n\n",
            if test { "from models import Item, Order\n" } else { "" }
        );
        t.push_str(&filler(&mut rng, (4 + i % 3) * scale));
        let pool = if test { TEST_CALLS } else { CALLS };
        // 各ファイルに 2 つの呼び出しを、補助関数の間に挟む。
        for k in 0..2 {
            t.push_str(pool[(i + k) % pool.len()]);
            t.push_str(&filler(&mut rng, 3 * scale));
        }
        files.insert(path.clone(), render(&t, n));
    }
    files.insert("docs/billing.md".to_string(), render(DOC, n));
    files
}

impl Refactor {
    pub(crate) fn new(seed: u64, xl: bool) -> Self {
        let mut original = build(seed ^ 0xfac7, &OLD, xl);
        original.insert("CHANGELOG.md".into(), CHANGELOG_OLD.into());
        let expected = build(seed ^ 0xfac7, &NEW, xl);
        Refactor {
            files: vfs::files(original.clone()),
            original,
            expected,
            done: Arc::new(Mutex::new(None)),
        }
    }
}

fn norm(s: &str) -> String {
    s.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// `calc_total` が単語として残っている箇所（似た名前の別関数は数えない）。
fn leftovers(body: &str) -> usize {
    let b = body.as_bytes();
    body.match_indices("calc_total")
        .filter(|(i, m)| {
            let before = i.checked_sub(1).map(|j| b[j]);
            let after = b.get(i + m.len()).copied();
            let word = |c: Option<u8>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_');
            !word(before) && !word(after)
        })
        .count()
}

/// 採点とテストの結果（失敗の説明の一覧）。
fn check(
    files: &BTreeMap<String, String>,
    r: &Refactor,
) -> (Vec<(&'static str, bool)>, Vec<String>) {
    let mut fails = Vec::new();
    let code = |p: &str| p != "CHANGELOG.md";
    // 1. 旧名が残っていない（CHANGELOG を除く）。
    let left: Vec<String> = files
        .iter()
        .filter(|(p, b)| code(p) && leftovers(b) > 0)
        .map(|(p, _)| p.clone())
        .collect();
    for p in &left {
        fails.push(format!(
            "FAILED {p} - NameError: name 'calc_total' is not defined"
        ));
    }
    // 2. 似た名前の別関数と、他の関数の tax= を壊していない。
    let count = |m: &BTreeMap<String, String>, pat: &str| -> usize {
        m.iter()
            .filter(|(p, _)| code(p))
            .map(|(_, b)| b.matches(pat).count())
            .sum()
    };
    let decoys_ok = [
        "calc_totals_by_day",
        "recalc_total",
        "format_receipt(total, tax=",
        "order.tax",
    ]
    .iter()
    .all(|d| count(files, d) == count(&r.original, d));
    if !decoys_ok {
        fails.push("FAILED tests/test_reports.py::test_related_helpers - calc_totals_by_day / recalc_total / format_receipt(tax=) / order.tax が変わっています".into());
    }
    // 3. 定義の署名と本体。
    let core_py = files.get("billing/core.py").map_or("", String::as_str);
    let sig_ok =
        core_py.contains(NEW.sig) && core_py.contains("return round(subtotal * (1 + tax_rate))");
    if !sig_ok {
        fails.push("FAILED tests/test_billing.py::test_signature - billing/core.py の定義が def compute_total(items, tax_rate, *, rounding=\"half_up\"): ではないか、本体が tax_rate を使っていません".into());
    }
    // 4. ファイルごとの完全一致（呼び出しのキーワード・ドキュメントを含む）。
    let mut mismatched = Vec::new();
    for (p, want) in &r.expected {
        if files.get(p).map(|g| norm(g)) != Some(norm(want)) {
            mismatched.push(p.clone());
        }
    }
    let extra: Vec<&String> = files
        .keys()
        .filter(|p| !r.original.contains_key(*p))
        .collect();
    for p in &mismatched {
        if !left.contains(p) {
            fails.push(format!("FAILED {p} - 期待される内容と一致しません（呼び出しのキーワード引数・コメント・文字列を確認）"));
        }
    }
    // 5. CHANGELOG: 過去の項目はそのまま・先頭に Unreleased の項目。
    let cl = files.get("CHANGELOG.md").map_or("", String::as_str);
    let old_kept = cl.contains(&CHANGELOG_OLD["# Changelog\n\n".len()..]);
    let unreleased = cl
        .find("## Unreleased")
        .zip(cl.find("## 1.4.0"))
        .is_some_and(|(u, o)| u < o && cl[u..o].contains("compute_total"));
    if !(old_kept && unreleased) {
        fails.push("FAILED CHANGELOG.md - 過去の項目を変えずに、先頭に ## Unreleased と compute_total への変更を書いてください".into());
    }
    if !extra.is_empty() {
        fails.push(format!("FAILED 余分なファイル: {extra:?}"));
    }
    let parts = vec![
        ("renamed_everywhere", left.is_empty()),
        ("decoys_intact", decoys_ok),
        ("signature", sig_ok),
        ("files_exact", mismatched.is_empty() && extra.is_empty()),
        ("changelog", old_kept && unreleased),
    ];
    (parts, fails)
}

impl Task for Refactor {
    fn system(&self) -> String {
        "あなたはソフトウェアエンジニアです。ファイルはツールで読み書きし、変更後は run_tests で確かめてください。".into()
    }

    fn prompt(&self) -> String {
        "このリポジトリで、関数 `calc_total(items, tax)` を次のように改めてください。\n\
         1. 名前を `compute_total` に変え、引数 `tax` を `tax_rate` に改名し、キーワード専用引数 \
         `rounding=\"half_up\"` を足す（定義は `def compute_total(items, tax_rate, *, rounding=\"half_up\"):`。\
         本体の `tax` も `tax_rate` にする。rounding は本体で使わなくてよい）。\n\
         2. すべての呼び出し・import・`__all__`・コメント・ログの文字列・docs/ のドキュメントも新しい名前にする。\
         `calc_total` の呼び出しで `tax=` と書いているキーワード引数は `tax_rate=` にする。\n\
         3. 名前の似た別の関数（`calc_totals_by_day`・`recalc_total`）と、他の関数の `tax=`（`format_receipt` など）、\
         属性 `order.tax` は変えない。\n\
         4. CHANGELOG.md の既存の項目は変えず、先頭（`# Changelog` の直後）に `## Unreleased` の節を作り、\
         この変更を 1 行で書く（`compute_total` という語を含める）。\n\
         変更が終わったら run_tests で確かめ、すべて通ったら finish を呼んでください。"
            .into()
    }

    fn max_steps(&self) -> usize {
        60
    }

    fn final_tool(&self) -> &'static str {
        "finish"
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut t = vfs::tools(&self.files, true);
        t.push(vfs::finish_tool(&self.done));
        let files = self.files.clone();
        let me = Refactor {
            original: self.original.clone(),
            expected: self.expected.clone(),
            files: self.files.clone(),
            done: self.done.clone(),
        };
        t.push(FnTool::new(
            "run_tests",
            "テストを実行して結果を返す。",
            json!({"type": "object", "properties": {}}),
            true,
            move |_| {
                let snapshot = files.lock().unwrap().clone();
                let (parts, fails) = check(&snapshot, &me);
                let passed = parts.iter().filter(|p| p.1).count();
                if fails.is_empty() {
                    Ok(format!("{passed} passed"))
                } else {
                    Ok(format!(
                        "{}\n{passed} passed, {} failed",
                        fails.join("\n"),
                        fails.len()
                    ))
                }
            },
        ));
        t
    }

    fn grade(&self) -> Grade {
        let snapshot = self.files.lock().unwrap().clone();
        let (parts, fails) = check(&snapshot, self);
        let weights = [0.3, 0.2, 0.15, 0.2, 0.15];
        let score: f64 = parts
            .iter()
            .zip(weights)
            .filter(|(p, _)| p.1)
            .map(|(_, w)| w)
            .sum();
        Grade {
            score,
            success: parts.iter().all(|p| p.1),
            details: json!({
                "parts": parts.iter().map(|(k, v)| json!({k.to_string(): v})).collect::<Vec<_>>(),
                "fails": fails,
                "finished": self.done.lock().unwrap().is_some(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_state_passes_and_original_fails() {
        let r = Refactor::new(3, false);
        let (parts, _) = check(&r.original, &r);
        assert!(!parts[0].1 && parts[1].1);
        let mut done = r.expected.clone();
        done.insert(
            "CHANGELOG.md".into(),
            CHANGELOG_OLD.replace(
                "# Changelog\n\n",
                "# Changelog\n\n## Unreleased\n- calc_total を compute_total に改名\n\n",
            ),
        );
        let (parts, fails) = check(&done, &r);
        assert!(parts.iter().all(|p| p.1), "{fails:?}");
        // 罠: 一括置換で別関数まで変えると落ちる。
        let mut naive = done.clone();
        for b in naive.values_mut() {
            *b = b.replace("calc_totals_by_day", "compute_totals_by_day");
        }
        assert!(!check(&naive, &r).0[1].1);
    }
}
