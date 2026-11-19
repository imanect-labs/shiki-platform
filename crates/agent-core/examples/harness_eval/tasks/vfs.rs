//! 編集系の課題で共有するメモリ上のファイル群とツール（一覧・行範囲の読み取り・検索・置換・書き込み）。
//!
//! 製品の fs_* ツールに近い形にする: 読み取りは行番号付き・範囲指定可、置換は一意な一致を要求し、
//! 一致しない／複数ある場合はエラーで返す（小さいモデルが雑な置換で壊すのを見るため）。

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use agent_core::Tool;
use serde_json::json;

use super::{arg_opt_u64, arg_str, FnTool};

pub(crate) type Files = Arc<Mutex<BTreeMap<String, String>>>;

pub(crate) fn files(init: BTreeMap<String, String>) -> Files {
    Arc::new(Mutex::new(init))
}

/// 1 回の読み取りで返す最大行数（既定はファイル全体＝文脈を圧迫する）。
const READ_MAX_LINES: usize = 2_000;
/// 検索で返す最大件数。
const SEARCH_MAX: usize = 80;

fn read(files: &Files, v: &serde_json::Value) -> Result<String, String> {
    let path = arg_str(v, "path")?;
    let fs = files.lock().unwrap();
    let Some(body) = fs.get(path) else {
        return Err(format!(
            "no such file: {path}（list_files で一覧を確認してください）"
        ));
    };
    let lines: Vec<&str> = body.lines().collect();
    let start = arg_opt_u64(v, "start_line")?.unwrap_or(1).max(1) as usize;
    let max = arg_opt_u64(v, "max_lines")?
        .map_or(READ_MAX_LINES, |m| m as usize)
        .clamp(1, READ_MAX_LINES);
    if start > lines.len() && !lines.is_empty() {
        return Err(format!("{path} は {} 行です", lines.len()));
    }
    let mut out = String::new();
    let end = (start - 1 + max).min(lines.len());
    for (i, l) in lines.iter().enumerate().take(end).skip(start - 1) {
        let _ = writeln!(out, "{:>5}| {l}", i + 1);
    }
    let _ = write!(
        out,
        "（{path}: {start}〜{end} 行目 / 全 {} 行）",
        lines.len()
    );
    Ok(out)
}

fn search(files: &Files, v: &serde_json::Value) -> Result<String, String> {
    let pattern = arg_str(v, "pattern")?;
    if pattern.is_empty() {
        return Err("pattern is empty".into());
    }
    let prefix = v.get("path").and_then(|p| p.as_str()).unwrap_or("");
    let fs = files.lock().unwrap();
    let mut hits = Vec::new();
    for (path, body) in fs.iter().filter(|(p, _)| p.starts_with(prefix)) {
        for (i, l) in body.lines().enumerate() {
            if l.contains(pattern) {
                hits.push(format!("{path}:{}: {l}", i + 1));
            }
        }
    }
    let total = hits.len();
    let mut out = hits
        .into_iter()
        .take(SEARCH_MAX)
        .collect::<Vec<_>>()
        .join("\n");
    let _ = write!(
        out,
        "\n（一致 {total} 件{}）",
        if total > SEARCH_MAX {
            format!("・先頭 {SEARCH_MAX} 件を表示。path で絞れます")
        } else {
            String::new()
        }
    );
    Ok(out)
}

fn edit(files: &Files, v: &serde_json::Value) -> Result<String, String> {
    let path = arg_str(v, "path")?;
    let old = arg_str(v, "old_string")?;
    let new = arg_str(v, "new_string")?;
    let all = v
        .get("replace_all")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if old.is_empty() {
        return Err("old_string is empty".into());
    }
    let mut fs = files.lock().unwrap();
    let Some(body) = fs.get_mut(path) else {
        return Err(format!("no such file: {path}"));
    };
    let n = body.matches(old).count();
    if n == 0 {
        return Err(format!(
            "old_string not found in {path}（空白・改行まで一致させてください。read_file で確認できます）"
        ));
    }
    if n > 1 && !all {
        return Err(format!(
            "old_string matches {n} places in {path}; 前後の行を含めて一意にするか、replace_all=true にしてください"
        ));
    }
    *body = if all {
        body.replace(old, new)
    } else {
        body.replacen(old, new, 1)
    };
    Ok(format!("{path}: {n} 箇所を置き換えました"))
}

fn write(files: &Files, v: &serde_json::Value) -> Result<String, String> {
    let path = arg_str(v, "path")?;
    let content = arg_str(v, "content")?;
    let mut fs = files.lock().unwrap();
    let existed = fs.insert(path.to_string(), content.to_string()).is_some();
    Ok(format!(
        "{path} を{}しました（{} 行）",
        if existed { "上書き" } else { "作成" },
        content.lines().count()
    ))
}

/// ファイル操作のツール一式。`writable` が偽なら読み取りだけ。
pub(crate) fn tools(files: &Files, writable: bool) -> Vec<Arc<dyn Tool>> {
    let f = files.clone();
    let mut out: Vec<Arc<dyn Tool>> = vec![
        FnTool::new(
            "list_files",
            "ファイルの一覧（パスと行数）。",
            json!({"type": "object", "properties": {}}),
            true,
            move |_| {
                let fs = f.lock().unwrap();
                Ok(fs
                    .iter()
                    .map(|(p, b)| format!("{p}（{} 行）", b.lines().count()))
                    .collect::<Vec<_>>()
                    .join("\n"))
            },
        ),
        {
            let f = files.clone();
            FnTool::new(
                "read_file",
                "ファイルを行番号付きで読む。start_line と max_lines で範囲を指定できる（省略するとファイル全体）。",
                json!({"type": "object", "properties": {
                    "path": {"type": "string"},
                    "start_line": {"type": "integer", "minimum": 1},
                    "max_lines": {"type": "integer", "minimum": 1}
                }, "required": ["path"]}),
                true,
                move |v| read(&f, v),
            )
        },
        {
            let f = files.clone();
            FnTool::new(
                "search",
                "全ファイルから文字列を探す（正規表現ではない・大文字小文字を区別する）。path を渡すと、そのパスで始まるファイルだけを探す。",
                json!({"type": "object", "properties": {
                    "pattern": {"type": "string"},
                    "path": {"type": "string", "description": "パスの前方一致で絞る（省略可）"}
                }, "required": ["pattern"]}),
                true,
                move |v| search(&f, v),
            )
        },
    ];
    if writable {
        let f = files.clone();
        out.push(FnTool::new(
            "edit_file",
            "ファイルの一部を置き換える。old_string はファイル内で一意に一致する必要がある（replace_all=true なら全部を置き換える）。",
            json!({"type": "object", "properties": {
                "path": {"type": "string"},
                "old_string": {"type": "string"},
                "new_string": {"type": "string"},
                "replace_all": {"type": "boolean"}
            }, "required": ["path", "old_string", "new_string"]}),
            false,
            move |v| edit(&f, v),
        ));
        let f = files.clone();
        out.push(FnTool::new(
            "write_file",
            "ファイルを丸ごと書き込む（新規作成または上書き）。",
            json!({"type": "object", "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            }, "required": ["path", "content"]}),
            false,
            move |v| write(&f, v),
        ));
    }
    out
}

/// 作業の終了を伝えるツール（編集系の課題の最後に呼ばせる）。
pub(crate) fn finish_tool(done: &Arc<Mutex<Option<String>>>) -> Arc<dyn Tool> {
    let d = done.clone();
    FnTool::new(
        "finish",
        "すべての変更を終えたら最後に 1 回だけ呼ぶ。summary に変更の要約を書く。",
        json!({"type": "object", "properties": {"summary": {"type": "string"}}, "required": ["summary"]}),
        false,
        move |v| {
            let s = arg_str(v, "summary")?;
            *d.lock().unwrap() = Some(s.to_string());
            Ok("完了を受け付けました。".into())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_requires_unique_match() {
        let f = files(BTreeMap::from([("a.py".into(), "x = 1\nx = 1\n".into())]));
        assert!(edit(
            &f,
            &json!({"path": "a.py", "old_string": "x = 1", "new_string": "y"})
        )
        .is_err());
        assert!(edit(
            &f,
            &json!({"path": "a.py", "old_string": "x = 1", "new_string": "y", "replace_all": true})
        )
        .is_ok());
        assert_eq!(f.lock().unwrap()["a.py"], "y\ny\n");
    }
}
