//! CoolWSD クライアントプロトコルの純関数層（issue #352）。
//!
//! ワイヤ形式は Collabora Online（distro/collabora/co-25.04）の
//! `wsd/protocol.txt` とブラウザ実装（`browser/js/global.js` /
//! `browser/src/app/{Socket,SearchService}.ts`）に一致させる:
//!
//! - WS URL: `/cool/<encodeURIComponent(WOPISrc?access_token=..)>/ws?WOPISrc=<enc>&compat=/ws`
//! - `load url=` は **access_token を含まない WOPISrc**（トークンは WS パス側が運ぶ）
//! - `unocommandresult:` は `.uno:Save` 等の限られたコマンドのみ。検索の成否は
//!   `searchnotfound:` / 選択コールバック＋`gettextselection` 照合で判定する
//! - paste の ack は `pasteresult: success|fallback`
//!
//! I/O を持たない純関数のみを置く（単体テストの主戦場）。

use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};

/// JS の `encodeURIComponent` と同じ非エスケープ集合（`A-Za-z0-9 - _ . ! ~ * ' ( )`）。
const URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

/// `encodeURIComponent` 相当。
fn encode_uri_component(s: &str) -> String {
    utf8_percent_encode(s, URI_COMPONENT).to_string()
}

/// ドキュメントセッションの WS URL を組み立てる。
///
/// ブラウザの `makeDocAndWopiSrcUrl` と同一形（末尾の `&compat=/ws` まで含めて一致）。
/// access_token はパス側の doc URL クエリとして焼き込む（coolwsd はこれを WOPI
/// 呼び出しに引き継ぐ）。
pub(super) fn session_ws_url(ws_base: &str, wopi_src: &str, access_token: &str) -> String {
    let doc_url_params = format!(
        "{wopi_src}?access_token={}",
        encode_uri_component(access_token)
    );
    format!(
        "{}/cool/{}/ws?WOPISrc={}&compat=/ws",
        ws_base.trim_end_matches('/'),
        encode_uri_component(&doc_url_params),
        encode_uri_component(wopi_src),
    )
}

/// 接続直後に送るプロトコル版宣言（`coolclient 0.1 <epoch_ms> <perf>`）。
///
/// timestamp/perfcounter はサーバ側トレースの時刻換算にのみ使われる。
pub(super) fn coolclient_line(now_epoch_ms: i64) -> String {
    format!("coolclient 0.1 {now_epoch_ms} 0")
}

/// ドキュメント load 要求。url は access_token を**含まない** WOPISrc。
pub(super) fn load_line(wopi_src: &str, lang: &str) -> String {
    format!(
        "load url={} lang={lang} deviceFormFactor=desktop",
        encode_uri_component(wopi_src)
    )
}

/// 自 view で `needle` を検索・選択する（`SearchItem.Command=0`＝find）。
///
/// キー名・型はブラウザの `SearchService.ts` と同一。成功時は見つかった箇所が
/// **この view の選択**になる（他 view の選択には影響しない）。
pub(super) fn uno_execute_search(needle: &str) -> String {
    let args = serde_json::json!({
        "SearchItem.SearchString": { "type": "string", "value": needle },
        "SearchItem.ReplaceString": { "type": "string", "value": "" },
        "SearchItem.Backward": { "type": "boolean", "value": false },
        "SearchItem.SearchStartPointX": { "type": "long", "value": 0 },
        "SearchItem.SearchStartPointY": { "type": "long", "value": 0 },
        "SearchItem.Command": { "type": "long", "value": 0 },
    });
    format!("uno .uno:ExecuteSearch {args}")
}

/// 自 view のセルカーソルを `cell_ref`（例 `A1`・`Sheet2.B3`・`A1:C4`）へ移す。
pub(super) fn uno_go_to_cell(cell_ref: &str) -> String {
    let args = serde_json::json!({
        "ToPoint": { "type": "string", "value": cell_ref },
    });
    format!("uno .uno:GoToCell {args}")
}

/// 文書末尾へカーソルを移す（Writer・append 用）。
pub(super) const UNO_GO_TO_END_OF_DOC: &str = "uno .uno:GoToEndOfDoc";

/// 自 view の選択内容を要求する（応答は `textselectioncontent: <raw>`）。
pub(super) const GET_TEXT_SELECTION_LINE: &str =
    "gettextselection mimetype=text/plain;charset=utf-8";

/// UNO save ラッパ（編集セッション非終了・未変更ならスキップ）。
pub(super) const SAVE_LINE: &str = "save dontTerminateEdit=1 dontSaveIfUnmodified=1";

/// `paste mimetype=<mime>\n<data>` のバイナリフレームを組み立てる。
pub(super) fn paste_frame(mime: &str, data: &[u8]) -> Vec<u8> {
    let header = format!("paste mimetype={mime}\n");
    let mut frame = Vec::with_capacity(header.len() + data.len());
    frame.extend_from_slice(header.as_bytes());
    frame.extend_from_slice(data);
    frame
}

/// `loaded:` の内容（view 確立の確定通知）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    /// この view の ID（LOKit 内で一意）。
    pub view_id: String,
    /// load 完了時点の総 view 数。
    pub views: u32,
    /// 最初の view（＝このセッションで文書が新規 load された）か。
    pub is_first: bool,
}

/// `loaded: viewid=<id> views=<n> isfirst=<bool>` をパースする。
pub(super) fn parse_loaded(line: &str) -> Option<Loaded> {
    let rest = line.strip_prefix("loaded:")?;
    let mut view_id = None;
    let mut views = None;
    let mut is_first = None;
    for token in rest.split_whitespace() {
        if let Some((name, value)) = token.split_once('=') {
            match name {
                "viewid" => view_id = Some(value.to_string()),
                "views" => views = value.parse().ok(),
                "isfirst" => is_first = Some(value == "true"),
                _ => {}
            }
        }
    }
    Some(Loaded {
        view_id: view_id?,
        views: views?,
        is_first: is_first?,
    })
}

/// `error: cmd=<c> kind=<k> ...` から (cmd, kind) を抜き出す。
pub(super) fn parse_error(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("error:")?;
    let mut cmd = None;
    let mut kind = None;
    // 2 行目以降の自由文は無視する（1 行目のみ見る）。
    for token in rest.lines().next()?.split_whitespace() {
        if let Some((name, value)) = token.split_once('=') {
            match name {
                "cmd" => cmd = Some(value.to_string()),
                "kind" => kind = Some(value.to_string()),
                _ => {}
            }
        }
    }
    Some((cmd?, kind.unwrap_or_default()))
}

/// `close: <reason>` の理由を抜き出す。
pub(super) fn parse_close_reason(line: &str) -> Option<String> {
    line.strip_prefix("close:").map(|r| r.trim().to_string())
}

/// `pasteresult: success|fallback` を成功可否に写す。
pub(super) fn parse_pasteresult(line: &str) -> Option<bool> {
    let rest = line.strip_prefix("pasteresult:")?;
    Some(rest.trim() == "success")
}

/// `unocommandresult: <json>` から (commandName, success) を抜き出す。
///
/// success は bool でも文字列 `"true"` でも受ける（LOK 実装差の吸収）。
pub(super) fn parse_unocommandresult(line: &str) -> Option<(String, bool)> {
    let json = line.strip_prefix("unocommandresult:")?.trim();
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let command = value.get("commandName")?.as_str()?.to_string();
    let success = match value.get("success") {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => s == "true",
        _ => false,
    };
    Some((command, success))
}

/// `textselectioncontent: <raw>` から選択内容を抜き出す。
///
/// 単一 mimetype 要求への応答は JSON でなく生テキスト（改行含み得る）。
pub(super) fn parse_textselectioncontent(msg: &str) -> Option<&str> {
    let rest = msg.strip_prefix("textselectioncontent:")?;
    // 先頭の区切り（スペース 1 個 or 改行）だけ剥がし、内容の空白は保存する。
    Some(
        rest.strip_prefix(' ')
            .or_else(|| rest.strip_prefix('\n'))
            .unwrap_or(rest),
    )
}

/// `set_cells` のアンカーとして許すセル参照か（`A1`・`Sheet2.B3`・`A1:C4`）。
///
/// LibreOffice に不正参照を渡すと GoToCell が無言で無視され、**現在位置に
/// 貼ってしまう**ため、送信前に構文で弾く（fail-closed）。シート名は
/// 英数と `_` のみ許す（空白・引用付きは非対応＝安全側）。
pub fn is_cell_ref(anchor: &str) -> bool {
    fn is_single_cell(s: &str) -> bool {
        let col_len = s.chars().take_while(char::is_ascii_uppercase).count();
        if !(1..=3).contains(&col_len) {
            return false;
        }
        let row = &s[col_len..];
        (1..=7).contains(&row.len()) && row.chars().all(|c| c.is_ascii_digit())
    }
    // 省略可能なシート接頭辞（最後の '.' で分ける）。
    let cell_part = match anchor.rsplit_once('.') {
        Some((sheet, rest)) => {
            if sheet.is_empty() || !sheet.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return false;
            }
            rest
        }
        None => anchor,
    };
    match cell_part.split_once(':') {
        Some((from, to)) => is_single_cell(from) && is_single_cell(to),
        None => is_single_cell(cell_part),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn ws_url_matches_browser_encoding() {
        let url = session_ws_url(
            "ws://collabora:9980",
            "http://shiki-server:8080/wopi/files/1234",
            "abc.def+/=",
        );
        // doc URL は「トークンをクエリ値としてエンコード→全体を再エンコード」の
        // 二重エンコード（ブラウザの docParams + makeDocAndWopiSrcUrl と同一挙動。
        // coolwsd がパスを 1 回デコードし、クエリ値のデコードは WOPI ホストが行う）。
        assert_eq!(
            url,
            "ws://collabora:9980/cool/http%3A%2F%2Fshiki-server%3A8080%2Fwopi%2Ffiles%2F1234%3Faccess_token%3Dabc.def%252B%252F%253D/ws?WOPISrc=http%3A%2F%2Fshiki-server%3A8080%2Fwopi%2Ffiles%2F1234&compat=/ws"
        );
    }

    #[test]
    fn load_line_excludes_access_token() {
        let line = load_line("http://shiki-server:8080/wopi/files/1234", "ja");
        assert_eq!(
            line,
            "load url=http%3A%2F%2Fshiki-server%3A8080%2Fwopi%2Ffiles%2F1234 lang=ja deviceFormFactor=desktop"
        );
        assert!(!line.contains("access_token"));
    }

    #[test]
    fn execute_search_uses_search_item_keys() {
        let line = uno_execute_search("見出し \"A\"\n次行");
        assert!(line.starts_with("uno .uno:ExecuteSearch {"));
        let json: serde_json::Value =
            serde_json::from_str(line.strip_prefix("uno .uno:ExecuteSearch ").unwrap()).unwrap();
        assert_eq!(
            json["SearchItem.SearchString"]["value"],
            "見出し \"A\"\n次行"
        );
        assert_eq!(json["SearchItem.Command"]["value"], 0);
        assert_eq!(json["SearchItem.Backward"]["value"], false);
    }

    #[test]
    fn go_to_cell_wraps_to_point() {
        let json: serde_json::Value = serde_json::from_str(
            uno_go_to_cell("Sheet2.B3")
                .strip_prefix("uno .uno:GoToCell ")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(json["ToPoint"]["value"], "Sheet2.B3");
    }

    #[test]
    fn paste_frame_prefixes_header() {
        let frame = paste_frame("text/html;charset=utf-8", b"<p>x</p>");
        assert!(frame.starts_with(b"paste mimetype=text/html;charset=utf-8\n"));
        assert!(frame.ends_with(b"<p>x</p>"));
    }

    #[test]
    fn parses_loaded() {
        assert_eq!(
            parse_loaded("loaded: viewid=7 views=2 isfirst=false"),
            Some(Loaded {
                view_id: "7".into(),
                views: 2,
                is_first: false
            })
        );
        // 必須フィールド欠落は None（プロトコル不整合として扱う）。
        assert_eq!(parse_loaded("loaded: viewid=7"), None);
        assert_eq!(parse_loaded("status: type=text"), None);
    }

    #[test]
    fn parses_error_and_close() {
        assert_eq!(
            parse_error("error: cmd=load kind=faileddocloading\n詳細"),
            Some(("load".into(), "faileddocloading".into()))
        );
        assert_eq!(
            parse_close_reason("close: documentconflict"),
            Some("documentconflict".into())
        );
        assert_eq!(parse_error("loaded: viewid=1"), None);
    }

    #[test]
    fn parses_pasteresult_and_unocommandresult() {
        assert_eq!(parse_pasteresult("pasteresult: success"), Some(true));
        assert_eq!(parse_pasteresult("pasteresult: fallback"), Some(false));
        assert_eq!(
            parse_unocommandresult(
                r#"unocommandresult: {"commandName":".uno:Save","success":true}"#
            ),
            Some((".uno:Save".into(), true))
        );
        assert_eq!(
            parse_unocommandresult(
                r#"unocommandresult: {"commandName":".uno:Save","success":"false"}"#
            ),
            Some((".uno:Save".into(), false))
        );
    }

    #[test]
    fn parses_textselectioncontent_preserving_body() {
        assert_eq!(
            parse_textselectioncontent("textselectioncontent: 一行目\n二行目"),
            Some("一行目\n二行目")
        );
        assert_eq!(
            parse_textselectioncontent("textselectioncontent: "),
            Some("")
        );
        assert_eq!(parse_textselectioncontent("complexselection:"), None);
    }

    #[test]
    fn cell_ref_validation() {
        for ok in [
            "A1",
            "AZ99",
            "AAA1048576",
            "Sheet2.B3",
            "data_1.C4",
            "A1:C4",
            "Sheet1.A1:B2",
        ] {
            assert!(is_cell_ref(ok), "{ok} は許可されるべき");
        }
        for bad in [
            "",
            "1A",
            "a1",
            "A",
            "12",
            "A1:",
            ":B2",
            "Sheet 1.A1",
            "'S'.A1",
            "A12345678",
            "=SUM(A1)",
        ] {
            assert!(!is_cell_ref(bad), "{bad} は拒否されるべき");
        }
    }
}
