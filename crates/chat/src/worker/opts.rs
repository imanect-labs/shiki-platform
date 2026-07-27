//! 実行オプションと system プロンプトの組み立て（generate.rs から分割）。
//!
//! プロファイル既定（Chat/Autonomous）と、**ユーザー由来文字列の無害化**を置く。

use agent_core::AgentOptions;

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

/// Chat プロファイルの実行オプション（制約版・現行挙動）。
pub(super) fn chat_opts(worker: &ChatWorker) -> AgentOptions {
    let mut opts = AgentOptions::chat(worker.config.max_steps);
    opts.system = Some(worker.config.system_prompt.clone());
    worker.config.model.clone_into(&mut opts.model);
    // 1 応答の出力上限（プロファイル既定 2048 は reasoning モデルの思考で尽き、
    // 長い成果物・大きなツール引数が途中で切れる。設定値で上書きする）。
    opts.max_tokens = Some(worker.config.max_tokens);
    opts.parallel_read_tools = worker.config.parallel_read_tools;
    opts
}

/// 自律プロファイルの system プロンプト（計画・ワークスペース・承認の作法を足す）。
pub(super) fn autonomous_system_prompt(base: &str) -> String {
    format!(
        "{base}\n\n\
         あなたは自律エージェントです。与えられた目標を達成するため、次の作法で進めてください:\n\
         - まず `plan` ツールで目標を数個のサブタスクに分解し、進捗に応じて計画を更新する。\n\
         - 作業ディレクトリ（ワークスペース）のファイルは fs_list/fs_read/grep で調べ、fs_write/fs_edit で編集する。\n\
         - コマンド実行が必要なら shell を使う（1 コマンドずつ・ネットワークは遮断）。\n\
         - 破壊的な操作（shell・削除）は承認が必要な場合がある。承認待ちで停止したら結果を待つ。\n\
         - 目標を達成したら簡潔に要約して終了する。"
    )
}
