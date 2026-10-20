//! 実行オプションと system プロンプトの組み立て（generate.rs から分割）。
//!
//! プロファイル既定（Chat/Autonomous）と、**ユーザー由来文字列の無害化**を置く。

use agent_core::{AgentOptions, ToolName};
use chrono::{DateTime, Datelike, Utc, Weekday};

use super::ChatWorker;

/// ユーザー由来の短い文字列を system プロンプトへ埋める前に無害化する。
///
/// 改行・制御文字を落として 1 行に潰し、長さを切る（プロンプトインジェクションの
/// 足場を作らない）。表示目的の付随情報であり、意味を厳密に保つ必要はない。
pub(super) fn sanitize_for_prompt(raw: &str) -> String {
    const MAX: usize = 120;
    let flat: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .filter(|c| *c != '「' && *c != '」')
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX {
        return flat;
    }
    flat.chars().take(MAX).collect::<String>() + "…"
}

/// system プロンプトの末尾に置く「今日の日付」（#546）。
///
/// 日付は **run の作成時刻**から決める（チェックポイントからの再開や、自律で日をまたいでも
/// 同じ run の中では変わらない）。時刻は入れない（毎回変わると prompt cache が効かない。
/// 日付だけなら変わるのは 1 日 1 回）。末尾に置き、ツール定義の cache は壊さない。
pub(crate) fn today_note(run_created_at: DateTime<Utc>, tz: chrono_tz::Tz) -> String {
    let d = run_created_at.with_timezone(&tz).date_naive();
    let weekday = match d.weekday() {
        Weekday::Mon => "月",
        Weekday::Tue => "火",
        Weekday::Wed => "水",
        Weekday::Thu => "木",
        Weekday::Fri => "金",
        Weekday::Sat => "土",
        Weekday::Sun => "日",
    };
    format!(
        "\n\n今日は {}年{}月{}日（{weekday}）です（{tz}）。「今日」「来週」「今月」などはこの日付を基準に解釈してください。",
        d.year(),
        d.month(),
        d.day()
    )
}

/// system プロンプトの末尾に、門の段階の説明（あれば）と今日の日付をこの順で足す。
pub(super) fn append_system_tail(opts: &mut AgentOptions, gate_note: Option<&str>, today: &str) {
    let mut system = opts.system.take().unwrap_or_default();
    if let Some(note) = gate_note {
        system.push_str(note);
    }
    system.push_str(today);
    opts.system = Some(system);
}

/// ユーザーが**この発話にだけ**指定したツール（#734）を実行オプションへ反映する。
///
/// - 提示されているもの（`offered`＝この run で実際に積んだツール名）だけを tool search の
///   常時ロードへ足す（指定したのに検索の 1 手の向こうに置かない）。
/// - system へ短い注記を足す（ユーザーの明示的な希望であること・関係する場面で使うこと）。
/// - 実行前フェーズの門（#400）が承認まで取り上げたもの（`held_by_gate`）は「承認後の実行
///   段階で使える」と添える（門の注記「承認すると次のターンで調査ツールが使える」と矛盾させない。
///   カード回答の続きの run へは指定が継がれる＝実行段階で効く・`chat.submit`）。
/// - それ以外で提示されていないもの（通常チャットの `code_interpreter` 未配線など）は
///   「この会話では使えない」とだけ添え、ユーザーへ説明できるようにする。
///
/// **誘導であって権限ではない**: ツールの追加・`opts.approval` の変更・認可の迂回はしない
/// （提示集合はプロファイル・skill・門が決めたまま。指定でツールが増えることはない）。
/// skill の指示文より後ろ（門の注記・日付より前）に置き、手順書の記述に上書きされないようにする。
pub(super) fn apply_requested_tools(
    opts: &mut AgentOptions,
    requested: &[ToolName],
    offered: &[&str],
    held_by_gate: &[&str],
) {
    if requested.is_empty() {
        return;
    }
    let (mut available, mut held, mut unavailable) = (Vec::new(), Vec::new(), Vec::new());
    for name in requested.iter().map(|t| t.as_str()) {
        if offered.contains(&name) {
            available.push(name);
        } else if held_by_gate.contains(&name) {
            held.push(name);
        } else {
            unavailable.push(name);
        }
    }
    opts.tool_search
        .always_load
        .extend(available.iter().map(|s| (*s).to_string()));
    let mut lines: Vec<String> = Vec::new();
    lines.push(if available.is_empty() {
        "ユーザーはこの発話でツールを指定しました。".to_string()
    } else {
        format!(
            "ユーザーはこの発話で次のツールを使うよう明示的に指定しています: {}。\
             依頼に関係する場面ではこれらを優先して使ってください（無関係なら無理に使わなくて構いません）。",
            available.join(", ")
        )
    });
    if !held.is_empty() {
        lines.push(format!(
            "次のツールは計画の承認後（実行段階）に使えます: {}。いまのフェーズでは使わず、\
             このフェーズでやることを進めてください（承認後の実行段階でユーザーの指定どおり優先して使います）。",
            held.join(", ")
        ));
    }
    if !unavailable.is_empty() {
        lines.push(format!(
            "次のツールはこの会話では使えません: {}。必要ならその旨をユーザーに伝えてください。",
            unavailable.join(", ")
        ));
    }
    let mut system = opts.system.take().unwrap_or_default();
    system.push_str("\n\n# この発話で指定されたツール\n");
    system.push_str(&lines.join("\n"));
    opts.system = Some(system);
}

/// 設定値のタイムゾーン名（IANA・例 `Asia/Tokyo`）を解釈する（不正なら起動時に明示エラー）。
pub fn parse_timezone(name: &str) -> Result<chrono_tz::Tz, String> {
    name.parse::<chrono_tz::Tz>()
        .map_err(|e| format!("タイムゾーン `{name}` を解釈できません（IANA 名で指定）: {e}"))
}

/// Chat プロファイルの実行オプション（制約版・現行挙動）。
pub(super) fn chat_opts(worker: &ChatWorker) -> AgentOptions {
    let mut opts = AgentOptions::chat(worker.config.max_steps);
    opts.system = Some(worker.config.system_prompt.clone());
    worker.config.model.clone_into(&mut opts.model);
    // 1 応答の出力上限（プロファイル既定 2048 は reasoning モデルの思考で尽き、
    // 長い成果物・大きなツール引数が途中で切れる。設定値で上書きする）。
    opts.max_tokens = Some(worker.config.max_tokens);
    opts.parallel_read_tools = worker.config.parallel_read_tools;
    opts.tool_search.enabled = worker.config.tool_search;
    opts.tool_search.embedder = worker.tool_search_embedder();
    opts
}

/// 自律プロファイルの system プロンプト（計画・ワークスペース・承認の作法を足す）。
pub(super) fn autonomous_system_prompt(base: &str) -> String {
    format!(
        "{base}\n\n\
         あなたは自律エージェントです。与えられた目標を達成するため、次の作法で進めてください:\n\
         - まず `plan` ツールで目標を数個のサブタスクに分解し、進捗に応じて計画を更新する。\n\
         - 作業ディレクトリ（ワークスペース）のファイルは fs_list/fs_read/grep で調べ、fs_write/fs_edit で編集する。\n\
         - コマンド実行が必要なら shell を使う（ネットワークは遮断・使える書き方はツール説明に従う）。\n\
         - 破壊的な操作（shell・削除）は承認が必要な場合がある。承認待ちで停止したら結果を待つ。\n\
         - 目標を達成したら簡潔に要約して終了する。"
    )
}

impl ChatWorker {
    /// tool search（と skill_search）の融合に使う埋め込み。RAG が配線されていれば同じもの、
    /// 無ければ `None`（BM25 のみ・#517）。
    pub(super) fn tool_search_embedder(
        &self,
    ) -> Option<std::sync::Arc<dyn rag::EmbeddingProvider>> {
        self.search.as_ref().map(|s| s.embedder())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn today_note_uses_the_configured_timezone_and_weekday() {
        // UTC では 10/13 の 16:30 だが、東京では 10/14（水）の 1:30。
        let at = Utc.with_ymd_and_hms(2026, 10, 13, 16, 30, 0).unwrap();
        let tokyo = today_note(at, chrono_tz::Asia::Tokyo);
        assert!(
            tokyo.contains("今日は 2026年10月14日（水）です（Asia/Tokyo）"),
            "{tokyo}"
        );
        let utc = today_note(at, chrono_tz::UTC);
        assert!(utc.contains("2026年10月13日（火）"), "{utc}");
        // 時刻は入れない（prompt cache を毎回壊さない）。
        assert!(
            !tokyo.contains("1:30") && !tokyo.contains("01:30"),
            "{tokyo}"
        );
    }

    #[test]
    fn parse_timezone_accepts_iana_names_and_rejects_others() {
        assert_eq!(parse_timezone("Asia/Tokyo"), Ok(chrono_tz::Asia::Tokyo));
        assert!(parse_timezone("JST+9").is_err());
        assert!(parse_timezone("").is_err());
    }

    #[test]
    fn requested_tools_are_loaded_and_noted_without_touching_approval() {
        let mut opts = AgentOptions::chat(1);
        opts.system = Some("基本".into());
        let approval_before = format!("{:?}", opts.approval);
        apply_requested_tools(
            &mut opts,
            &[ToolName::WebSearch, ToolName::CsvQuery],
            &["doc_search", "web_search", "csv.query"],
            &[],
        );
        assert_eq!(
            opts.tool_search.always_load,
            vec!["web_search".to_string(), "csv.query".to_string()]
        );
        let system = opts.system.as_deref().unwrap();
        assert!(
            system.starts_with("基本\n\n# この発話で指定されたツール\n"),
            "{system}"
        );
        assert!(system.contains("web_search, csv.query"), "{system}");
        assert!(!system.contains("使えません"), "{system}");
        // 誘導であって権限ではない（承認ポリシは変えない）。
        assert_eq!(format!("{:?}", opts.approval), approval_before);
    }

    #[test]
    fn requested_tools_not_offered_are_neither_loaded_nor_granted() {
        // 通常チャットで shell を指定しても提示されない（常時ロードにも載せない＝昇格しない）。
        let mut opts = AgentOptions::chat(1);
        apply_requested_tools(
            &mut opts,
            &[ToolName::Shell, ToolName::DocSearch],
            &["doc_search"],
            &[],
        );
        assert_eq!(opts.tool_search.always_load, vec!["doc_search".to_string()]);
        let system = opts.system.as_deref().unwrap();
        assert!(system.contains("指定しています: doc_search。"), "{system}");
        assert!(
            system.contains("次のツールはこの会話では使えません: shell。"),
            "{system}"
        );

        let mut none_offered = AgentOptions::chat(1);
        apply_requested_tools(&mut none_offered, &[ToolName::Shell], &[], &[]);
        assert!(none_offered.tool_search.always_load.is_empty());
        let system = none_offered.system.as_deref().unwrap();
        assert!(
            system.contains("指定しました。\n次のツールはこの会話では使えません: shell。"),
            "{system}"
        );
    }

    /// 門（#400）が承認まで取り上げたツールは「使えない」ではなく「承認後に使える」と伝える
    /// （門の注記と矛盾させない）。常時ロードにも載せない（提示していないため）。
    #[test]
    fn requested_tools_held_by_the_gate_are_noted_as_after_approval() {
        let mut opts = AgentOptions::chat(1);
        apply_requested_tools(
            &mut opts,
            &[
                ToolName::WebSearch,
                ToolName::DocSearch,
                ToolName::CodeInterpreter,
            ],
            &["doc_search", "fs_write"],
            &["web_search", "web_fetch"],
        );
        assert_eq!(opts.tool_search.always_load, vec!["doc_search".to_string()]);
        let system = opts.system.as_deref().unwrap();
        assert!(system.contains("指定しています: doc_search。"), "{system}");
        assert!(
            system.contains("次のツールは計画の承認後（実行段階）に使えます: web_search。"),
            "{system}"
        );
        assert!(!system.contains("使えません: web_search"), "{system}");
        // 門と無関係に提示されていないものは従来どおり「使えない」。
        assert!(
            system.contains("次のツールはこの会話では使えません: code_interpreter。"),
            "{system}"
        );

        // 指定がすべて門で保留されている（明確化の段階）。
        let mut clarify = AgentOptions::chat(1);
        apply_requested_tools(
            &mut clarify,
            &[ToolName::WebSearch, ToolName::DocSearch],
            &[],
            &["web_search", "doc_search", "fs_write"],
        );
        assert!(clarify.tool_search.always_load.is_empty());
        let system = clarify.system.as_deref().unwrap();
        assert!(
            system.contains("指定しました。\n次のツールは計画の承認後（実行段階）に使えます: web_search, doc_search。"),
            "{system}"
        );
        assert!(!system.contains("使えません"), "{system}");
    }

    #[test]
    fn no_requested_tools_is_a_no_op() {
        let mut opts = AgentOptions::chat(1);
        opts.system = Some("基本".into());
        apply_requested_tools(&mut opts, &[], &["doc_search"], &["web_search"]);
        assert_eq!(opts.system.as_deref(), Some("基本"));
        assert!(opts.tool_search.always_load.is_empty());
    }

    #[test]
    fn tail_puts_the_gate_note_before_the_date() {
        let mut opts = AgentOptions::chat(1);
        opts.system = Some("基本".into());
        append_system_tail(&mut opts, Some("\n\n# フェーズ"), "\n\n今日は");
        assert_eq!(opts.system.as_deref(), Some("基本\n\n# フェーズ\n\n今日は"));
        let mut none = AgentOptions::chat(1);
        append_system_tail(&mut none, None, "今日は");
        assert_eq!(none.system.as_deref(), Some("今日は"));
    }
}
