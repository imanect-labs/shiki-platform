//! 評価データ（`eval/decision/datasets/*.jsonl`）と質問（`*.questions.json`）の読み込み、段の定義。

use std::collections::BTreeMap;
use std::path::PathBuf;

use llm_gateway::{Effort, Question, QuestionKind};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// 評価するデータの名前（ファイル名の頭）。
pub(crate) const DATASETS: [&str; 2] = ["inquiry_triage", "expense_policy"];

/// `eval/decision`（リポジトリの直下）。
pub(crate) fn eval_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../eval/decision")
}

/// 評価データの 1 行。ラベルは作り方から決まる（`eval/decision/build_datasets.py`）。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Row {
    pub(crate) id: String,
    pub(crate) split: String,
    /// 判定材料（そのまま `DecideRequest::state` に渡す）。
    pub(crate) state: Value,
    /// 質問のキー → 正解。
    pub(crate) labels: BTreeMap<String, Value>,
    pub(crate) tags: Value,
    /// 経費の規程のフラグ（script が出すものと同じ役割・問い合わせには無い）。
    #[serde(default)]
    pub(crate) rule_flags: Vec<String>,
}

impl Row {
    pub(crate) fn label_str(&self, key: &str) -> Option<&str> {
        self.labels.get(key).and_then(Value::as_str)
    }

    pub(crate) fn label_bool(&self, key: &str) -> Option<bool> {
        self.labels.get(key).and_then(Value::as_bool)
    }

    pub(crate) fn tag_bool(&self, key: &str) -> bool {
        self.tags.get(key).and_then(Value::as_bool).unwrap_or(false)
    }

    pub(crate) fn tag_str(&self, key: &str) -> Option<&str> {
        self.tags.get(key).and_then(Value::as_str)
    }
}

/// 評価データ 1 つ（行と質問）。
#[derive(Debug, Clone)]
pub(crate) struct Dataset {
    pub(crate) name: &'static str,
    pub(crate) rows: Vec<Row>,
    pub(crate) questions: Vec<Question>,
    /// 質問のファイルの sha256（凍結の確かめに使う）。
    pub(crate) questions_sha256: String,
    /// 行のファイル（`*.jsonl`）の sha256（どのデータで採点したかを結果に残す）。
    pub(crate) rows_sha256: String,
}

impl Dataset {
    pub(crate) fn row(&self, id: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.id == id)
    }

    /// 段階の質問の段階の名前（低い順）。
    pub(crate) fn levels(&self, key: &str) -> Vec<String> {
        self.questions
            .iter()
            .find(|q| q.key == key)
            .and_then(|q| match &q.kind {
                QuestionKind::Score { levels } => Some(levels.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }
}

/// 評価データと質問を読む（形が崩れていれば止める）。
pub(crate) fn load(name: &'static str) -> Dataset {
    let dir = eval_dir().join("datasets");
    let rows_text = std::fs::read_to_string(dir.join(format!("{name}.jsonl")))
        .unwrap_or_else(|e| panic!("{name}.jsonl を読めない: {e}"));
    let rows_sha256 = hex(&Sha256::digest(rows_text.as_bytes()));
    let rows = rows_text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| {
            serde_json::from_str::<Row>(l)
                .unwrap_or_else(|e| panic!("{name}.jsonl の {} 行目: {e}", i + 1))
        })
        .collect();
    let questions_text = std::fs::read(dir.join(format!("{name}.questions.json")))
        .unwrap_or_else(|e| panic!("{name}.questions.json を読めない: {e}"));
    let questions = serde_json::from_slice(&questions_text)
        .unwrap_or_else(|e| panic!("{name}.questions.json: {e}"));
    Dataset {
        name,
        rows,
        questions,
        questions_sha256: hex(&Sha256::digest(&questions_text)),
        rows_sha256,
    }
}

/// 名前（結果のファイルに書いたもの）から評価データを読む。
pub(crate) fn load_named(name: &str) -> Dataset {
    let name = DATASETS
        .iter()
        .find(|n| **n == name)
        .unwrap_or_else(|| panic!("評価データ {name} は無い（{DATASETS:?}）"));
    load(name)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// 評価の段（付録 B §6）。dev で決めて test は凍結する。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Stage {
    pub(crate) name: &'static str,
    pub(crate) split: &'static str,
    pub(crate) reps: u32,
    pub(crate) effort: Effort,
    pub(crate) samples: u8,
    /// 凍結（`eval/decision/frozen.json`）が要るか（test を使う段）。
    pub(crate) frozen: bool,
}

/// 段の名前から構成を引く。
pub(crate) fn stage(name: &str) -> Option<Stage> {
    let (name, split, reps, effort, samples) = match name {
        // dev のベースライン（k=1・low）
        "E0" => ("E0", "dev", 1, Effort::Low, 1),
        // dev でのプロンプトの改訂と θ の選定（改訂ごとに走らせる）
        "E4" => ("E4", "dev", 1, Effort::Low, 1),
        // test・k=1・low・3 回反復
        "E1" => ("E1", "test", 3, Effort::Low, 1),
        // test・k=1・medium（max_tokens 4096）
        "E2" => ("E2", "test", 1, Effort::Medium, 1),
        // test・k=5・low
        "E3" => ("E3", "test", 1, Effort::Low, 5),
        _ => return None,
    };
    Some(Stage {
        name,
        split,
        reps,
        effort,
        samples,
        frozen: split == "test",
    })
}

/// 凍結したプロンプトの版としきい値（E4 の後に書く・test の段はこれを読む）。
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub(crate) struct Frozen {
    /// llm_decider のプロンプトの版（`resolved_model` の `llm_decider/<版>/`）。
    pub(crate) prompt_version: String,
    /// データ名 → しきい値。
    pub(crate) theta: BTreeMap<String, f64>,
    /// データ名 → 質問のファイルの sha256。
    pub(crate) questions_sha256: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) based_on: String,
    /// 凍結の理由（θ の決め方など）。
    #[serde(default)]
    pub(crate) notes: String,
}

pub(crate) fn frozen() -> Option<Frozen> {
    let text = std::fs::read_to_string(eval_dir().join("frozen.json")).ok()?;
    Some(serde_json::from_str(&text).unwrap_or_else(|e| panic!("frozen.json: {e}")))
}

/// `resolved_model` からプロンプトの版を取り出す（`…+llm_decider/v2/k1/low` → `v2`）。
pub(crate) fn prompt_version_of(resolved_model: &str) -> Option<&str> {
    resolved_model
        .split_once("+llm_decider/")
        .and_then(|(_, rest)| rest.split('/').next())
}

/// 答えた版（`resolved_model`）の一覧（重複を除いて辞書順）。
pub(crate) fn resolved_models(calls: &[crate::run::CallResult]) -> Vec<String> {
    calls
        .iter()
        .filter_map(|c| c.resolved_model.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// 答えた版のどれかが凍結した版と違えば、その説明（凍結の無い段・答えの無い実行は `None`）。
pub(crate) fn version_mismatch(resolved: &[String], frozen: Option<&Frozen>) -> Option<String> {
    let f = frozen?;
    let other: Vec<&String> = resolved
        .iter()
        .filter(|r| prompt_version_of(r) != Some(f.prompt_version.as_str()))
        .collect();
    (!other.is_empty()).then(|| {
        format!(
            "答えた版（{other:?}）が凍結した版 {} と違う",
            f.prompt_version
        )
    })
}
