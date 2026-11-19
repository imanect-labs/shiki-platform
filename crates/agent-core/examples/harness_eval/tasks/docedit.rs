//! 長い文書の編集課題: 社内規程のハンドブック（Markdown・約 1 万字）に 4 つの編集を入れる。
//!
//! - 節の改名と、目次・本文中のリンク（`[経費精算](#経費精算)`）の追従。
//! - 1 つの節の中だけで金額を改める（同じ文言は他の節にもあり、変えてはいけない）。
//! - 節の新設（決まった位置・目次にも足す）と、節の削除（目次と他の節からのリンクも外す）。
//!
//! 期待される最終形は節の列から組み立て、正規化して完全一致で比べる（部分点は編集ごと）。

use std::sync::{Arc, Mutex};

use agent_core::Tool;
use serde_json::json;

use super::{vfs, Grade, Rng, Task};

const PATH: &str = "handbook.md";

const SECTIONS: &[&str] = &[
    "勤務時間",
    "休暇",
    "通勤手当",
    "出張",
    "経費精算",
    "FAX による申請",
    "備品の購入",
    "情報セキュリティ",
    "評価面談",
    "研修",
    "副業",
    "退職の手続き",
];

const SENTENCES: &[&str] = &[
    "申請は原則として前営業日までに行ってください。",
    "所属長の承認を得たうえで、人事部に届け出ます。",
    "やむを得ない事情がある場合は、事後の申請も認めます。",
    "記録は 5 年間保存し、監査の求めに応じて提出します。",
    "不明な点は総務部の窓口に問い合わせてください。",
    "制度の詳細は四半期ごとに見直します。",
    "対象となるのは正社員および契約社員です。",
    "申請内容に誤りがあった場合は、速やかに訂正してください。",
];

/// `-xl` で足す節（文書を約 3 倍にする）。
const EXTRA_SECTIONS: &[&str] = &[
    "健康診断",
    "育児休業",
    "介護休業",
    "慶弔休暇",
    "社内表彰",
    "服務規律",
    "ハラスメントの防止",
    "個人情報の取り扱い",
    "社用車の利用",
    "名刺の発注",
    "郵便物の取り扱い",
    "会議室の予約",
    "入館証の管理",
    "災害時の対応",
    "社内システムの利用",
    "ソフトウェアの導入",
    "契約書の管理",
    "広報への問い合わせ",
];

const NEW_SECTION: &str = "## 在宅勤務手当\n\n在宅勤務を月 10 日以上行った月は、在宅勤務手当として 3,000円を支給します。\n申請は不要で、勤怠の記録から自動で計算します。\n";

pub(crate) struct DocEdit {
    expected: String,
    original: String,
    files: vfs::Files,
    done: Arc<Mutex<Option<String>>>,
}

fn anchor(title: &str) -> String {
    title.to_lowercase().replace(' ', "-")
}

/// 文書を組み立てる。`edited` なら 4 つの編集を入れた形。
fn build(seed: u64, edited: bool, xl: bool) -> String {
    let mut rng = Rng::new(seed);
    let mut sections: Vec<&str> = SECTIONS.to_vec();
    if xl {
        // 退職の手続きの前に差し込む（編集する節の位置関係は変えない）。
        let at = sections.len() - 1;
        sections.splice(at..at, EXTRA_SECTIONS.iter().copied());
    }
    let paragraphs = if xl { 6 } else { 4 };
    let title = |t: &str| -> String {
        if edited && t == "経費精算" {
            "経費の精算手続き".into()
        } else {
            t.into()
        }
    };
    let link = |t: &str| -> String {
        if edited && t == "FAX による申請" {
            "FAX による申請（廃止）".into()
        } else {
            let t = title(t);
            format!("[{t}](#{})", anchor(&t))
        }
    };
    let mut doc = String::from(
        "# 社内規程ハンドブック\n\nこの文書は社内の各種規程をまとめたものです。\n\n## 目次\n\n",
    );
    for s in &sections {
        if edited && *s == "FAX による申請" {
            continue;
        }
        doc.push_str(&format!("- {}\n", link(s)));
        if edited && *s == "通勤手当" {
            doc.push_str("- [在宅勤務手当](#在宅勤務手当)\n");
        }
    }
    doc.push('\n');
    for (i, s) in sections.iter().enumerate() {
        let body = {
            let mut b = String::new();
            for p in 0..paragraphs {
                for _ in 0..rng.range(4, 7) {
                    b.push_str(rng.pick(SENTENCES));
                }
                // 金額の文言（同じ文言が複数の節にある）。
                if p == 1 && matches!(*s, "出張" | "経費精算" | "備品の購入") {
                    let amount = if edited && *s == "出張" {
                        "8,000円"
                    } else {
                        "5,000円"
                    };
                    b.push_str(&format!(
                        "1 回あたりの上限 {amount}を超える場合は、事前に承認を得てください。"
                    ));
                }
                // 他の節への参照。
                if p == 2 && i % 3 == 0 {
                    b.push_str(&format!("（詳しくは{}を参照）", link("経費精算")));
                }
                if p == 3 && i % 4 == 1 {
                    b.push_str(&format!("（紙の申請は{}を参照）", link("FAX による申請")));
                }
                b.push_str("\n\n");
            }
            b
        };
        if edited && *s == "FAX による申請" {
            continue;
        }
        doc.push_str(&format!("## {}\n\n{body}", title(s)));
        if edited && *s == "通勤手当" {
            doc.push_str(NEW_SECTION);
            doc.push('\n');
        }
    }
    doc
}

fn norm(s: &str) -> String {
    s.lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

impl DocEdit {
    pub(crate) fn new(seed: u64, xl: bool) -> Self {
        let original = build(seed ^ 0xd0c, false, xl);
        let expected = build(seed ^ 0xd0c, true, xl);
        DocEdit {
            files: vfs::files([(PATH.to_string(), original.clone())].into()),
            expected,
            original,
            done: Arc::new(Mutex::new(None)),
        }
    }

    fn parts(&self, doc: &str) -> Vec<(&'static str, bool)> {
        let n = norm(doc);
        let renamed = n.contains("## 経費の精算手続き")
            && !n.contains("(#経費精算)")
            && !n.contains("## 経費精算")
            && n.matches("[経費の精算手続き](#経費の精算手続き)").count()
                == self
                    .expected
                    .matches("[経費の精算手続き](#経費の精算手続き)")
                    .count();
        let amount = n.matches("8,000円").count() == 1
            && n.matches("5,000円").count() == self.expected.matches("5,000円").count()
            && section_of(&n, "## 出張").contains("8,000円");
        let added = section_of(&n, "## 在宅勤務手当").contains("3,000円")
            && n.find("## 通勤手当")
                .zip(n.find("## 在宅勤務手当"))
                .zip(n.find("## 出張"))
                .is_some_and(|((a, b), c)| a < b && b < c)
            && n.contains("- [在宅勤務手当](#在宅勤務手当)");
        let removed = !n.contains("## FAX による申請")
            && !n.contains("(#fax-による申請)")
            && n.matches("FAX による申請（廃止）").count()
                == self.expected.matches("FAX による申請（廃止）").count();
        vec![
            ("renamed_with_links", renamed),
            ("amount_in_one_section", amount),
            ("section_added", added),
            ("section_removed", removed),
            ("exact", n == norm(&self.expected)),
        ]
    }
}

fn section_of<'a>(doc: &'a str, heading: &str) -> &'a str {
    let Some(at) = doc.find(heading) else {
        return "";
    };
    let rest = &doc[at + heading.len()..];
    &rest[..rest.find("\n## ").unwrap_or(rest.len())]
}

impl Task for DocEdit {
    fn system(&self) -> String {
        "あなたは文書の編集担当です。ファイルはツールで読み書きし、指示された箇所以外は変えないでください。".into()
    }

    fn prompt(&self) -> String {
        format!(
            "{PATH}（社内規程のハンドブック）に、次の 4 つの編集を入れてください。\n\
             1. 節「## 経費精算」を「## 経費の精算手続き」に改名し、目次と本文中のこの節へのリンクをすべて\
             `[経費の精算手続き](#経費の精算手続き)` に直す。\n\
             2. 節「## 出張」の中の「上限 5,000円」だけを「上限 8,000円」に改める（他の節の 5,000円は変えない）。\n\
             3. 節「## 通勤手当」の直後（次の節の前）に、次の節をそのまま足し、目次の「通勤手当」の次の行に \
             `- [在宅勤務手当](#在宅勤務手当)` を足す。\n```\n{NEW_SECTION}```\n\
             4. 節「## FAX による申請」を見出しから次の節の手前まで丸ごと削除し、目次の項目も消す。\
             他の節にある `[FAX による申請](#fax-による申請)` というリンクは、リンクを外して「FAX による申請（廃止）」\
             という文字列に置き換える。\n\
             編集が終わったら finish を呼んでください。"
        )
    }

    fn max_steps(&self) -> usize {
        40
    }

    fn final_tool(&self) -> &'static str {
        "finish"
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut t = vfs::tools(&self.files, true);
        t.push(vfs::finish_tool(&self.done));
        t
    }

    fn grade(&self) -> Grade {
        let doc = self
            .files
            .lock()
            .unwrap()
            .get(PATH)
            .cloned()
            .unwrap_or_default();
        let parts = self.parts(&doc);
        let score = parts[..4].iter().filter(|p| p.1).count() as f64 * 0.2
            + if parts[4].1 { 0.2 } else { 0.0 };
        let _ = &self.original;
        Grade {
            score,
            success: parts.iter().all(|p| p.1),
            details: json!({
                "parts": parts.iter().map(|(k, v)| json!({k.to_string(): v})).collect::<Vec<_>>(),
                "finished": self.done.lock().unwrap().is_some(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_passes_original_fails() {
        let d = DocEdit::new(5, true);
        assert!(d.parts(&d.expected).iter().all(|p| p.1));
        assert!(d.parts(&d.original).iter().all(|p| !p.1));
        assert!(d.original.lines().count() > 60);
    }
}
