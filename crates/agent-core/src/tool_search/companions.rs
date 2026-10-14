//! 読み込むツールの ID 引数を調べる下調べ用ツール（#565）。
//!
//! 実行するツール（`outlook_delete_event`）だけを読み込むと、その必須引数の ID（`event_id`）を
//! 返す一覧・検索のツール（`outlook_search_events`）が読み込まれず、モデルは利用者に ID を
//! 尋ねて終えがち（#545）。結果の本文で名前を案内するだけでは追加の検索をしなかった（#522）
//! ので、一緒に読み込む。
//!
//! 対応は名前と引数名だけで決める: 必須引数 `<対象>_id` / `<対象>_key`（`id` だけなら名前の
//! 最後の語が対象）に対し、同じ名前空間で、名前に一覧・検索の動詞と対象（複数形も可）を含み、
//! 自分ではその引数を要しないツール。

use llm_gateway::ToolDef;

/// 1 回の検索で一緒に読み込む下調べ用ツールの上限（読み込む定義のトークンを抑える）。
pub(super) const MAX_COMPANIONS: usize = 4;
/// 1 つの ID 引数あたりの候補の上限。
const PER_ARG: usize = 2;
/// 下調べに使える動詞（前ほど優先）。`get` は ID を受け取る取得が多いので最後。
const LOOKUP_VERBS: [&str; 4] = ["search", "list", "find", "get"];

/// 1 つの下調べ用ツール（どのツールのどの引数のためか）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Companion {
    pub(super) tool: usize,
    pub(super) arg: String,
}

fn tokens(name: &str) -> Vec<String> {
    name.split(['.', '_', '-'])
        .filter(|t| !t.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn required(def: &ToolDef) -> Vec<String> {
    def.input_schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|r| {
            r.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// ID の形の引数なら、その対象の語（`event_id` → `event`・`id` → 名前の最後の語）。
fn entity(arg: &str, name: &[String]) -> Option<String> {
    let a = arg.to_ascii_lowercase();
    let stem = a
        .strip_suffix("_id")
        .or_else(|| a.strip_suffix("_key"))
        .or_else(|| (a == "id" || a == "key").then_some(""))?;
    let last = stem.rsplit('_').next().unwrap_or("");
    if last.is_empty() {
        name.last().cloned()
    } else {
        Some(last.to_string())
    }
}

/// 語が対象（単数・複数形）に当たるか。
fn names_entity(token: &str, entity: &str) -> bool {
    token == entity
        || token.strip_suffix('s') == Some(entity)
        || token.strip_suffix("es") == Some(entity)
        || entity
            .strip_suffix('y')
            .is_some_and(|stem| token.strip_suffix("ies") == Some(stem))
}

/// 各ツールの下調べ用ツール（定義と同じ並び・優先順）。
pub(super) fn build(defs: &[ToolDef]) -> Vec<Vec<Companion>> {
    let toks: Vec<Vec<String>> = defs.iter().map(|d| tokens(&d.name)).collect();
    let reqs: Vec<Vec<String>> = defs.iter().map(required).collect();
    defs.iter()
        .enumerate()
        .map(|(i, _)| {
            let mut out: Vec<Companion> = Vec::new();
            for arg in &reqs[i] {
                let Some(ent) = entity(arg, &toks[i]) else {
                    continue;
                };
                let mut found: Vec<(usize, usize)> = (0..defs.len())
                    .filter(|&j| j != i && toks[j].first() == toks[i].first())
                    .filter(|&j| !reqs[j].contains(arg))
                    .filter(|&j| toks[j].iter().skip(1).any(|t| names_entity(t, &ent)))
                    .filter_map(|j| {
                        LOOKUP_VERBS
                            .iter()
                            .position(|v| toks[j].iter().skip(1).any(|t| t == v))
                            .map(|rank| (rank, j))
                    })
                    .collect();
                found.sort_unstable();
                for (_, j) in found.into_iter().take(PER_ARG) {
                    if !out.iter().any(|c| c.tool == j) {
                        out.push(Companion {
                            tool: j,
                            arg: arg.clone(),
                        });
                    }
                }
            }
            out
        })
        .collect()
}

/// 読み込む `hits` に足す下調べ用ツール（幅優先・下調べ用ツール自身の ID 引数もたどる・
/// `hits` と重複しない・[`MAX_COMPANIONS`] 件まで）。戻り値は (下調べ用ツール, 対象のツール, 引数)。
pub(super) fn expand(table: &[Vec<Companion>], hits: &[usize]) -> Vec<(usize, usize, String)> {
    let mut out: Vec<(usize, usize, String)> = Vec::new();
    let mut queue: std::collections::VecDeque<usize> = hits.iter().copied().collect();
    while let Some(i) = queue.pop_front() {
        for c in table.get(i).into_iter().flatten() {
            if out.len() >= MAX_COMPANIONS {
                return out;
            }
            if hits.contains(&c.tool) || out.iter().any(|(t, _, _)| *t == c.tool) {
                continue;
            }
            out.push((c.tool, i, c.arg.clone()));
            queue.push_back(c.tool);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn def(name: &str, required: &[&str]) -> ToolDef {
        ToolDef::new(
            name,
            "d",
            json!({"type": "object", "properties": {}, "required": required}),
        )
    }

    fn names(defs: &[ToolDef], got: &[(usize, usize, String)]) -> Vec<String> {
        got.iter().map(|(j, _, _)| defs[*j].name.clone()).collect()
    }

    #[test]
    fn id_args_pull_in_list_and_search_tools_of_the_same_namespace() {
        let defs = vec![
            def("outlook_delete_event", &["event_id"]),
            def("outlook_search_events", &["query"]),
            def("outlook_list_events", &[]),
            def("outlook_get_event", &["event_id"]),
            def("outlook_list_emails", &[]),
            def("gcal_list_events", &[]),
        ];
        let table = build(&defs);
        let got = expand(&table, &[0]);
        // 検索 → 一覧の順。ID を要する取得・別の対象・別の名前空間は入れない。
        assert_eq!(
            names(&defs, &got),
            ["outlook_search_events", "outlook_list_events"]
        );
        assert_eq!(got[0].1, 0);
        assert_eq!(got[0].2, "event_id");
    }

    #[test]
    fn chains_follow_the_companions_own_id_args_up_to_the_cap() {
        let defs = vec![
            def("jira_update_sprint", &["sprint_id"]),
            def("jira_list_sprints", &["board_id"]),
            def("jira_list_boards", &[]),
            def("jira_get_board", &["board_id"]),
        ];
        let got = expand(&build(&defs), &[0]);
        assert_eq!(
            names(&defs, &got),
            ["jira_list_sprints", "jira_list_boards"]
        );
        assert_eq!(got[1].1, 1, "ボードはスプリント一覧のために読む");
    }

    #[test]
    fn plural_forms_bare_id_and_dotted_names_match() {
        let defs = vec![
            def("tracker.close_story", &["id"]),
            def("tracker.search_stories", &[]),
            def("figma_delete_comment", &["file_key", "comment_id"]),
            def("figma_search_files", &[]),
            def("figma_get_comments", &["file_key"]),
        ];
        let table = build(&defs);
        assert_eq!(
            names(&defs, &expand(&table, &[0])),
            ["tracker.search_stories"]
        );
        assert_eq!(
            names(&defs, &expand(&table, &[2])),
            ["figma_search_files", "figma_get_comments"]
        );
    }

    #[test]
    fn no_companions_without_id_args_and_no_duplicates_of_hits() {
        let defs = vec![
            def("csv.patch", &["path", "ops"]),
            def("csv.query", &["path", "sql"]),
            def("trello_remove_member", &["card_id"]),
            def("trello_list_cards", &[]),
        ];
        let table = build(&defs);
        assert!(expand(&table, &[0, 1]).is_empty());
        assert!(
            expand(&table, &[2, 3]).is_empty(),
            "既に読み込むものは足さない"
        );
    }

    #[test]
    fn expansion_is_capped() {
        let mut defs = vec![def("svc_do", &["a_id", "b_id", "c_id"])];
        for e in ["a", "b", "c"] {
            defs.push(def(&format!("svc_list_{e}s"), &[]));
            defs.push(def(&format!("svc_search_{e}s"), &[]));
        }
        assert_eq!(expand(&build(&defs), &[0]).len(), MAX_COMPANIONS);
    }
}
