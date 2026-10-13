//! 実行オプションと system プロンプトの組み立て（generate.rs から分割）。
//!
//! プロファイル既定（Chat/Autonomous）と、**ユーザー由来文字列の無害化**を置く。

use agent_core::AgentOptions;
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
