//! 決定的スタブの固定応答（`yui:` 駆動の ```ui の図・`long:` / `longthink:` 駆動の長文）。
//!
//! [`super::stub`] から分離したフィクスチャ群。ファイルサイズと関心の分離のため独立させる。

/// `yui:<kind>` の応答本文（```ui ブロックの図を含む Markdown・#530）。
///
/// `bar` / `line` / `stat` / `flow` / `stack` はそれぞれの図、`all` は全種類、`bad` は図にできない
/// ブロック（コードブロック表示へのフォールバック検証用）。`app <名前>`・`form <名前>`（会話に
/// 送るボタンの無い app）・`ask`・`plan` は操作を持つ図。
pub(super) fn yui_reply(kind: &str) -> String {
    const BAR: &str = "```ui\nbar 四半期ごとの売上と利益\nunit: 百万円\n四半期 | 売上 | 利益\nQ1 | 120 | 30\nQ2 | 150 | -5\nQ3 | 135 | 12\nQ4 | 210 | 44\n```";
    const LINE: &str = "```ui\nline 週次アクティブユーザー\n週 | Web | アプリ\n第1週 | 1200 | 800\n第2週 | 1350 | 950\n第3週 | 1280 | 1100\n第4週 | 1500 | 1300\n```";
    const STAT: &str = "```ui\nstat 今月の営業実績\n売上 | 3,200万円 | 前年比 +18%\n新規顧客数 | 165社\n解約率 | 2.2% | 前月 2.6%\n```";
    const FLOW: &str = "```ui\nflow 経費精算\n精算書提出 -> 上長承認\n上長承認 -承認-> 経理確認 -> 支払\n上長承認 -差戻し-> 修正\n修正 -> 精算書提出\n```";
    const STACK: &str = "```ui\nstack システム構成\n- 画面: Web / スマホ\n- API サーバ\n  - 認証\n  - 検索\n- データベース | ファイル保管\nnote: 論理構成。\n```";
    // 操作を持つ種類（#604）: ボタンの経路（束縛・検証・起動の記録）の e2e 用。
    const ASK: &str = "```ui\nask 進め方の確認\n? 対象の地域は\n- 関東\n- 関西\n? 重視する点 multi\n- 費用\n- 速さ\n- その他\n```";
    const PLAN: &str =
        "```ui\nplan 調査の計画\n1. 市場規模を調べる\n2. 競合を比べる\n3. 報告にまとめる\n```";
    let body = match kind {
        k if k == "app" || k.starts_with("app ") => {
            let name = k.strip_prefix("app").unwrap_or("").trim();
            let name = if name.is_empty() { "expense" } else { name };
            format!(
                "```ui\napp 経費の申請\n## 申請\n金額 = number 必須 円 as amount\n区分 = choice 交通費 | 交際費 as category\nbutton 申請する -> workflow {name}\n## 相談\n内容 = textarea 必須\nbutton 相談する -> chat\n```"
            )
        }
        // ワークフローを起動するボタンだけの app（「アプリとして保存」の e2e 用・#604）。
        k if k.starts_with("form ") => {
            let name = k.strip_prefix("form").unwrap_or("").trim();
            format!(
                "```ui\napp 経費の申請\n金額 = number 必須 円 as amount\n区分 = choice 交通費 | 交際費 as category\nbutton 申請する -> workflow {name}\n```"
            )
        }
        "ask" => ASK.to_string(),
        "plan" => PLAN.to_string(),
        "line" => LINE.to_string(),
        "stat" => STAT.to_string(),
        "flow" => FLOW.to_string(),
        "stack" => STACK.to_string(),
        "all" => [BAR, LINE, STAT, FLOW, STACK].join("\n\n"),
        "bad" => "```ui\nこれは図ではありません\n```".to_string(),
        _ => BAR.to_string(),
    };
    format!("集計結果を図にしました。\n\n{body}\n\n以上です。")
}

/// `long:<段落数>` の応答本文（見出し付きの長い Markdown）。
///
/// 生成中に読み返す（上へスクロールしても追従で引き戻されない）ことを e2e で確かめるための長文。
/// 段落ごとに番号を振るので、読んでいた段落が動いていないかを本文から見分けられる。
pub(super) fn long_reply(paragraphs: usize) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("長い回答を書きます。\n\n");
    for i in 1..=paragraphs {
        if i % 5 == 1 {
            let _ = write!(out, "## 第{}章\n\n", i / 5 + 1);
        }
        let _ = write!(
            out,
            "段落{i}。これは生成中に上から読み進められるかを確かめるための長い文章です。\
             読んでいる位置が保たれていれば、この段落は追従で画面の外へ押し流されません。\
             下端にいるときだけ、新しい段落に合わせて画面が下へ追いかけます。\n\n"
        );
    }
    out.push_str("以上です。");
    out
}

/// `longthink:<段落数>` の思考（番号付きの長い思考テキスト）。
///
/// 思考パネルを開いたまま、思考が流れている最中に読み返せることを e2e で確かめるための長文。
pub(super) fn long_thinking(paragraphs: usize) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for i in 1..=paragraphs {
        let _ = write!(
            out,
            "思考{i}。質問の意図を整理し、どの順で説明すると読みやすいかを検討している。\
             前の段落との重複を避けつつ、根拠になる情報を確かめてから次へ進む。\n\n"
        );
    }
    out
}
