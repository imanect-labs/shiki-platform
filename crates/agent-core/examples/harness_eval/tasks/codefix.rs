//! コード修正課題: 仮想リポジトリの失敗テストを、テストを変えずに直す。
//!
//! 3 種のバグ（設定値・境界条件・改名漏れ 2 か所）を、読み取り・検索・完全一致置換の編集・
//! テスト実行で直させる。ファイルは長め（`read_file` は 1 回 150 行まで）にして、
//! 読み取りの積み重ねで文脈が埋まるようにする。テストの書き換えは不正として 0 点。

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use agent_core::Tool;
use serde_json::json;

use super::{arg_opt_u64, arg_str, FnTool, Grade, Task};

type Files = Arc<Mutex<BTreeMap<String, String>>>;

fn filler_functions(prefix: &str, n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        let _ = write!(
            s,
            "\n\ndef {prefix}_helper_{i}(values, scale=1.0):\n    \
             \"\"\"Return the scaled sum of `values` for report section {i}.\n\n    \
             This helper is kept for backward compatibility with the {prefix} reports.\n    \
             \"\"\"\n    total = 0.0\n    for v in values:\n        \
             if v is None:\n            continue\n        total += float(v) * scale\n    \
             return round(total, 2)\n"
        );
    }
    s
}

fn initial_files() -> BTreeMap<String, String> {
    let mut f = BTreeMap::new();
    f.insert(
        "README.md".into(),
        "# shop\n\nSmall order/billing library.\n\n- `DEFAULT_TIMEOUT` is 30 seconds.\n\
         - Orders of 10,000 yen or more get a 5% discount.\n- Names are normalized with \
         `normalize_name` (American spelling).\n\nRun the test suite with the `run_tests` tool.\n"
            .into(),
    );
    f.insert(
        "src/shop/config.py".into(),
        "\"\"\"Configuration constants.\"\"\"\n\nDEFAULT_TIMEOUT = 3  # seconds\n\
         RETRY_COUNT = 3\nTAX_RATE = 0.10\nDISCOUNT_THRESHOLD = 10000\nDISCOUNT_RATE = 0.05\n\
         CURRENCY = \"JPY\"\n"
            .into(),
    );
    f.insert(
        "src/shop/utils.py".into(),
        format!(
            "\"\"\"Utility helpers.\"\"\"\n\n\ndef normalize_name(name):\n    \
             \"\"\"Trim and title-case a person name.\"\"\"\n    \
             return \" \".join(part.capitalize() for part in name.strip().split())\n\n\n\
             def format_yen(amount):\n    return f\"{{amount:,.0f}} JPY\"\n{}",
            filler_functions("utils", 12)
        ),
    );
    f.insert(
        "src/shop/billing.py".into(),
        format!(
            "\"\"\"Billing: tax and discounts.\"\"\"\n\nfrom .config import TAX_RATE, \
             DISCOUNT_THRESHOLD, DISCOUNT_RATE\n{}\n\n\ndef calculate_tax(amount):\n    \
             return round(amount * TAX_RATE, 2)\n\n\ndef apply_discount(amount):\n    \
             \"\"\"Apply the volume discount to `amount`.\"\"\"\n    \
             if amount > DISCOUNT_THRESHOLD:\n        \
             return round(amount * (1 - DISCOUNT_RATE), 2)\n    return float(amount)\n{}",
            filler_functions("billing", 14),
            filler_functions("billing_extra", 10)
        ),
    );
    f.insert(
        "src/shop/users.py".into(),
        format!(
            "\"\"\"User records.\"\"\"\n\nfrom .utils import normalise_name\n{}\n\n\n\
             def create_user(name, email):\n    \
             return {{\"name\": normalise_name(name), \"email\": email.lower()}}\n",
            filler_functions("users", 6)
        ),
    );
    f.insert(
        "src/shop/orders.py".into(),
        format!(
            "\"\"\"Orders.\"\"\"\n\nfrom .billing import apply_discount, calculate_tax\n\
             from .utils import normalise_name\n{}\n\n\ndef create_order(customer, amount):\n    \
             discounted = apply_discount(amount)\n    return {{\n        \
             \"customer\": normalise_name(customer),\n        \"amount\": discounted,\n        \
             \"tax\": calculate_tax(discounted),\n    }}\n{}",
            filler_functions("orders", 8),
            filler_functions("orders_extra", 6)
        ),
    );
    f.insert(
        "tests/test_config.py".into(),
        "from shop.config import DEFAULT_TIMEOUT, TAX_RATE\n\n\ndef test_default_timeout():\n    \
         assert DEFAULT_TIMEOUT == 30\n\n\ndef test_tax_rate():\n    assert TAX_RATE == 0.10\n"
            .into(),
    );
    f.insert(
        "tests/test_billing.py".into(),
        "from shop.billing import apply_discount, calculate_tax\n\n\n\
         def test_discount_at_threshold():\n    assert apply_discount(10000) == 9500.0\n\n\n\
         def test_no_discount_below_threshold():\n    assert apply_discount(9999) == 9999.0\n\n\n\
         def test_tax():\n    assert calculate_tax(1000) == 100.0\n"
            .into(),
    );
    f.insert(
        "tests/test_users.py".into(),
        "from shop.users import create_user\n\n\ndef test_create_user():\n    \
         assert create_user(\"  ada lovelace \", \"ADA@EXAMPLE.COM\") == {\"name\": \"Ada Lovelace\", \"email\": \"ada@example.com\"}\n"
            .into(),
    );
    f.insert(
        "tests/test_orders.py".into(),
        "from shop.orders import create_order\n\n\ndef test_create_order():\n    \
         assert create_order(\"bob\", 20000)[\"customer\"] == \"Bob\"\n"
            .into(),
    );
    f
}

pub(crate) struct CodeFix {
    files: Files,
    original: BTreeMap<String, String>,
}

impl CodeFix {
    pub(crate) fn new() -> Self {
        let f = initial_files();
        CodeFix {
            original: f.clone(),
            files: Arc::new(Mutex::new(f)),
        }
    }
}

/// テスト結果（名前, 成否, 失敗時の説明）。ファイル内容から決定的に判定する。
fn evaluate(files: &BTreeMap<String, String>) -> Vec<(&'static str, bool, String)> {
    let get = |p: &str| files.get(p).cloned().unwrap_or_default();
    let config = get("src/shop/config.py");
    let billing = get("src/shop/billing.py");
    let utils = get("src/shop/utils.py");
    let users = get("src/shop/users.py");
    let orders = get("src/shop/orders.py");
    let has_line = |src: &str, line: &str| src.lines().any(|l| l.trim() == line);
    let timeout = config
        .lines()
        .find_map(|l| l.trim().strip_prefix("DEFAULT_TIMEOUT"))
        .and_then(|r| r.trim().strip_prefix('='))
        .map(|r| r.split('#').next().unwrap_or("").trim().to_string())
        .unwrap_or_default();
    // 名前の解決を素直に模す: utils が定義する名前（def / 代入の別名）、各モジュールが
    // utils から import する名前、呼び出す名前。呼ぶ名前を import していて、utils がそれを
    // 定義していれば通る（定義側を改名する直し方も、呼び出し側を直す直し方も正しい）。
    let defined: Vec<String> = utils
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            l.strip_prefix("def ")
                .and_then(|r| r.split('(').next())
                .or_else(|| l.split_once(" = ").map(|(a, _)| a))
                .map(|n| n.trim().to_string())
        })
        .collect();
    let name_ok = |src: &str, line: usize| -> (bool, String) {
        let imported: Vec<String> = src
            .lines()
            .filter_map(|l| l.trim().strip_prefix("from .utils import "))
            .flat_map(|r| r.split(',').map(|n| n.trim().to_string()))
            .collect();
        let called: Vec<&str> = ["normalize_name(", "normalise_name("]
            .into_iter()
            .filter(|c| {
                src.lines()
                    .any(|l| !l.trim_start().starts_with("from ") && l.contains(c))
            })
            .map(|c| c.trim_end_matches('('))
            .collect();
        let ok = !called.is_empty()
            && called
                .iter()
                .all(|c| imported.iter().any(|i| i == c) && defined.iter().any(|d| d == c));
        let missing = called
            .iter()
            .find(|c| !(imported.iter().any(|i| i == *c) && defined.iter().any(|d| d == *c)))
            .unwrap_or(&"normalise_name");
        (
            ok,
            format!("NameError: name '{missing}' is not defined (line {line})"),
        )
    };
    let threshold_ok = billing.contains("if amount >= DISCOUNT_THRESHOLD:")
        || billing.contains("if DISCOUNT_THRESHOLD <= amount:");
    let below_ok = !billing.contains("amount > DISCOUNT_THRESHOLD - ")
        && has_line(&billing, "return float(amount)");
    let (users_ok, users_msg) = name_ok(&users, 3);
    let (orders_ok, orders_msg) = name_ok(&orders, 4);
    let mut out = vec![
        (
            "tests/test_config.py::test_default_timeout",
            timeout == "30",
            format!("AssertionError: assert {timeout} == 30"),
        ),
        (
            "tests/test_config.py::test_tax_rate",
            has_line(&config, "TAX_RATE = 0.10"),
            "AssertionError: TAX_RATE changed".into(),
        ),
        (
            "tests/test_billing.py::test_discount_at_threshold",
            threshold_ok,
            "AssertionError: assert apply_discount(10000) == 9500.0 (got 10000.0)".into(),
        ),
        (
            "tests/test_billing.py::test_no_discount_below_threshold",
            below_ok,
            "AssertionError: assert apply_discount(9999) == 9999.0".into(),
        ),
        (
            "tests/test_billing.py::test_tax",
            billing.contains("def calculate_tax(amount):")
                && billing.contains("return round(amount * TAX_RATE, 2)"),
            "AssertionError: calculate_tax(1000) != 100.0".into(),
        ),
        ("tests/test_users.py::test_create_user", users_ok, users_msg),
        (
            "tests/test_orders.py::test_create_order",
            orders_ok,
            orders_msg,
        ),
    ];
    // 既存の補助関数を壊していない（丸ごと書き換えて消していない）こと。
    for (path, prefix, n) in [
        ("src/shop/billing.py", "billing", 14),
        ("src/shop/orders.py", "orders", 8),
        ("src/shop/utils.py", "utils", 12),
    ] {
        let src = get(path);
        let ok =
            (0..n).all(|i| src.contains(&format!("def {prefix}_helper_{i}(values, scale=1.0):")));
        out.push((
            match prefix {
                "billing" => "tests/test_compat.py::test_billing_helpers",
                "orders" => "tests/test_compat.py::test_orders_helpers",
                _ => "tests/test_compat.py::test_utils_helpers",
            },
            ok,
            format!("ImportError: helper functions missing from {path}"),
        ));
    }
    out
}

impl Task for CodeFix {
    fn system(&self) -> String {
        "あなたはソフトウェアエンジニアのアシスタントです。リポジトリはツール越しにだけ操作できます。".into()
    }

    fn prompt(&self) -> String {
        "このリポジトリのテストが失敗しています。run_tests で失敗を確認し、原因を調べて src/ 以下の\
         コードを修正し、すべてのテストを通してください。tests/ 以下のテストは変更しないでください。\
         最後に run_tests で全件成功を確認してから終了してください。"
            .into()
    }

    fn max_steps(&self) -> usize {
        30
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let (f1, f2, f3, f4, f5, f6) = (
            Arc::clone(&self.files),
            Arc::clone(&self.files),
            Arc::clone(&self.files),
            Arc::clone(&self.files),
            Arc::clone(&self.files),
            Arc::clone(&self.files),
        );
        vec![
            FnTool::new(
                "list_files",
                "リポジトリ内の全ファイルのパスと行数を返す。",
                json!({"type": "object", "properties": {}}),
                true,
                move |_| {
                    let files = f1.lock().unwrap();
                    Ok(files
                        .iter()
                        .map(|(p, c)| format!("{p} ({} lines)", c.lines().count()))
                        .collect::<Vec<_>>()
                        .join("\n"))
                },
            ),
            FnTool::new(
                "read_file",
                "ファイルを行番号付きで読む。1 回に最大 150 行。start_line（1 始まり）と end_line で範囲を指定できる。\
                 行番号と「| 」は表示用で、ファイルの内容には含まれない。",
                json!({"type": "object", "properties": {
                    "path": {"type": "string"},
                    "start_line": {"type": "integer"},
                    "end_line": {"type": "integer"}
                }, "required": ["path"]}),
                true,
                move |v| {
                    let path = arg_str(v, "path")?;
                    let start = arg_opt_u64(v, "start_line")?.unwrap_or(1).max(1) as usize;
                    let files = f2.lock().unwrap();
                    let content = files.get(path).ok_or_else(|| format!("file not found: {path}"))?;
                    let lines: Vec<&str> = content.lines().collect();
                    let end = arg_opt_u64(v, "end_line")?
                        .map_or(start + 149, |e| e as usize)
                        .min(start + 149)
                        .min(lines.len());
                    let mut out = String::new();
                    for (i, l) in lines.iter().enumerate().take(end).skip(start - 1) {
                        let _ = writeln!(out, "{:>4} | {l}", i + 1);
                    }
                    if end < lines.len() {
                        let _ = writeln!(out, "... ({} more lines; use start_line={} to continue)", lines.len() - end, end + 1);
                    }
                    Ok(out)
                },
            ),
            FnTool::new(
                "search",
                "全ファイルから部分文字列を検索し、`path:line: text` を最大 50 件返す。",
                json!({"type": "object", "properties": {"pattern": {"type": "string"}}, "required": ["pattern"]}),
                true,
                move |v| {
                    let pat = arg_str(v, "pattern")?;
                    let files = f3.lock().unwrap();
                    let hits: Vec<String> = files
                        .iter()
                        .flat_map(|(p, c)| {
                            c.lines()
                                .enumerate()
                                .filter(|(_, l)| l.contains(pat))
                                .map(move |(i, l)| format!("{p}:{}: {l}", i + 1))
                        })
                        .take(50)
                        .collect();
                    Ok(if hits.is_empty() { "no matches".into() } else { hits.join("\n") })
                },
            ),
            FnTool::new(
                "edit_file",
                "ファイル中の old_string を new_string に置き換える。old_string はファイル内で一意に一致する必要がある\
                 （空白・インデントも含めて完全一致。行番号は含めない）。",
                json!({"type": "object", "properties": {
                    "path": {"type": "string"},
                    "old_string": {"type": "string"},
                    "new_string": {"type": "string"}
                }, "required": ["path", "old_string", "new_string"]}),
                false,
                move |v| {
                    let path = arg_str(v, "path")?;
                    let old = arg_str(v, "old_string")?;
                    let new = arg_str(v, "new_string")?;
                    let mut files = f4.lock().unwrap();
                    let content = files.get_mut(path).ok_or_else(|| format!("file not found: {path}"))?;
                    match content.matches(old).count() {
                        0 => Err(format!("old_string not found in {path}. It must match the file exactly (whitespace included, no line numbers). Read the file again.")),
                        1 => {
                            *content = content.replacen(old, new, 1);
                            Ok(format!("edited {path}"))
                        }
                        n => Err(format!("old_string matches {n} places in {path}; include more surrounding lines to make it unique.")),
                    }
                },
            ),
            FnTool::new(
                "write_file",
                "ファイル全体を content で上書きする（新規作成も可）。",
                json!({"type": "object", "properties": {
                    "path": {"type": "string"}, "content": {"type": "string"}
                }, "required": ["path", "content"]}),
                false,
                move |v| {
                    let path = arg_str(v, "path")?;
                    let content = arg_str(v, "content")?;
                    f5.lock().unwrap().insert(path.into(), content.into());
                    Ok(format!("wrote {path} ({} lines)", content.lines().count()))
                },
            ),
            FnTool::new(
                "run_tests",
                "テストを実行し、失敗したテストと件数を返す。",
                json!({"type": "object", "properties": {}}),
                false,
                move |_| {
                    let files = f6.lock().unwrap();
                    let results = evaluate(&files);
                    let failed: Vec<String> = results
                        .iter()
                        .filter(|(_, ok, _)| !ok)
                        .map(|(n, _, m)| format!("FAILED {n} - {m}"))
                        .collect();
                    let passed = results.len() - failed.len();
                    Ok(format!("{}\n{} failed, {passed} passed", failed.join("\n"), failed.len()))
                },
            ),
        ]
    }

    fn grade(&self) -> Grade {
        let files = self.files.lock().unwrap();
        let results = evaluate(&files);
        let tests_modified: Vec<&String> = self
            .original
            .iter()
            .filter(|(p, _)| p.starts_with("tests/"))
            .filter(|(p, c)| files.get(*p) != Some(*c))
            .map(|(p, _)| p)
            .collect();
        let passed = results.iter().filter(|(_, ok, _)| *ok).count();
        let score = if tests_modified.is_empty() {
            passed as f64 / results.len() as f64
        } else {
            0.0
        };
        Grade {
            score,
            success: tests_modified.is_empty() && passed == results.len(),
            details: json!({
                "failed": results.iter().filter(|(_, ok, _)| !ok).map(|(n, _, _)| *n).collect::<Vec<_>>(),
                "tests_modified": tests_modified,
            }),
        }
    }
}
