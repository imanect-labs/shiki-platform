//! 中継の通行判定（純ロジック・単体テスト対象）。
//!
//! 2 つの判定を持つ:
//! - [`route_for`]: ブラウザからの HTTP 要求のうち、**Collabora の編集画面が実際に使う経路だけ**を
//!   通す（許可リスト）。中継を挟むと Collabora から見た接続元は shiki-server（内部 IP）になるため、
//!   `net.post_allow.host` で内部からだけ許していた REST（convert-to・extract/transform-document-structure）や
//!   管理画面（adminws・getMetrics）が、素通しでは外部から叩けてしまう。拒否リストではなく許可リストに
//!   するのはこのため（上流が新しい REST を足しても、明示的に足すまで通らない）。
//! - [`client_frame_verdict`]: 編集セッションの WebSocket でブラウザが送るフレームのうち、
//!   kit 内でスクリプトを実行させる命令（`executescript` / `proxyreturn`）を落とす。
//!   Collabora 26.04 はこれを**閲覧専用セッションからも無条件に**実行する（実機確認・PIT-68）。
//!   スクリプトを送ってよいのは AI 参加者（中継を通らない内部接続）だけ。

/// 中継が通す経路の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// 通常の HTTP として上流へ転送する。
    Http,
    /// 編集セッションの WebSocket（`/cool/<doc>/ws`）。フレーム検査つきで中継する。
    DocumentSocket,
}

/// ブラウザから kit へ送られると、kit 内でスクリプトが実行される命令。
///
/// `executescript` は QuickJS 上で完全な UNO API を持つ JS を走らせ、`proxyreturn` は
/// その JS が待つ戻り値を注入する（どちらも Collabora 26.04 で追加・`wsd/protocol.txt`）。
/// 上流の版上げで同種の命令が増えたら、ここへ足して単体テストを増やす（PIT-68 ④）。
pub const BLOCKED_CLIENT_COMMANDS: &[&str] = &["executescript", "proxyreturn"];

/// ブラウザ → Collabora のフレームを通してよいか。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameVerdict {
    /// 通す。
    Allow,
    /// 落とす（接続ごと切る）。中身は拒否した命令名。
    Block(&'static str),
}

/// HTTP 要求の経路を判定する。`None` は拒否（404 を返す）。
///
/// `path` はパーセントデコード**前**の生のパス（クエリを含まない）。
pub fn route_for(method: &str, path: &str, is_websocket_upgrade: bool) -> Option<Route> {
    // `..` や二重スラッシュで判定をすり抜ける余地を残さない（上流の解釈と食い違い得る形は拒否）。
    // パーセントエンコードした `%2e%2e` も、上流へ渡す URL の組み立て（url クレート）が `..` として
    // 畳むので、各要素をデコードしてから見る。`%2f` で 1 要素に詰めた区切りもデコード後に分けて見る。
    if path.contains("..") || path.contains("//") || path.contains('\\') {
        return None;
    }
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let decoded: Vec<String> = segments
        .iter()
        .map(|s| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8_lossy()
                .into_owned()
        })
        .collect();
    let parts = || decoded.iter().flat_map(|s| s.split(['/', '\\']));
    if parts().any(|p| p == "." || p == "..") {
        return None;
    }
    let read_only = matches!(method, "GET" | "HEAD");
    match segments.as_slice() {
        // 編集画面の HTML・JS・CSS・画像。管理画面（/browser/dist/admin/）だけは通さない。
        // cool.html はトークンを form POST で受け取るので POST も通す。
        ["browser", rest @ ..] if !rest.is_empty() => {
            let is_admin = parts().any(|p| p.eq_ignore_ascii_case("admin"));
            let is_editor_page = rest
                .last()
                .is_some_and(|s| s.to_ascii_lowercase().ends_with(".html"));
            (!is_admin && (read_only || (method == "POST" && is_editor_page)))
                .then_some(Route::Http)
        }
        // discovery・capabilities・favicon と、文書内の動画・音声（/cool/media）。
        ["hosting", "discovery" | "capabilities"] | ["favicon.ico"] | ["cool", "media"]
            if read_only =>
        {
            Some(Route::Http)
        }
        // コピー＆ペーストの本体（クエリに WOPISrc・ViewId・Tag）。
        ["cool", "clipboard"] if read_only || method == "POST" => Some(Route::Http),
        // 編集セッション（26.04 のブラウザは `/cool/ws?WOPISrc=…&access_token=…` の形で張る）。
        ["cool", "ws"] if is_websocket_upgrade && method == "GET" => Some(Route::DocumentSocket),
        // 編集セッション（旧形・AI 参加者と同じ `/cool/<doc>/ws`）。WebSocket のアップグレードだけを通す。
        // `/cool/<doc>/ws/<session>/<command>/<serial>`（HTTP ロングポーリング版の
        // ProxyProtocol）はフレーム検査を経ずに命令を運べるので、ここでは一致させない。
        ["cool", doc, "ws"]
            if is_document_segment(doc) && is_websocket_upgrade && method == "GET" =>
        {
            Some(Route::DocumentSocket)
        }
        // 「名前を付けて保存」「印刷」の書き出し。
        ["cool", doc, "download", id]
            if is_document_segment(doc) && read_only && !id.is_empty() =>
        {
            Some(Route::Http)
        }
        // ローカル画像の挿入（ブラウザがファイルを POST する）。
        ["cool", doc, "insertfile"] if is_document_segment(doc) && method == "POST" => {
            Some(Route::Http)
        }
        _ => None,
    }
}

/// `/cool/<doc>/...` の `<doc>` が、パーセントエンコードされた WOPISrc の形か。
fn is_document_segment(segment: &str) -> bool {
    let lower = segment.to_ascii_lowercase();
    lower.starts_with("http%3a%2f%2f") || lower.starts_with("https%3a%2f%2f")
}

/// ブラウザ → Collabora の 1 フレームを検査する。
///
/// coolwsd は 1 行目を空白で区切った先頭トークンを命令名として扱う。先頭の空白を読み飛ばす
/// 実装もあり得るので、こちらも ASCII 空白を読み飛ばしてから先頭トークンを取り、大文字小文字を
/// 無視して照合する（判定を上流より緩くしない）。テキストでもバイナリでも同じ規則で見る
/// （`paste` などはバイナリで届く）。
pub fn client_frame_verdict(frame: &[u8]) -> FrameVerdict {
    let start = frame
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(frame.len());
    let rest = &frame[start..];
    let end = rest
        .iter()
        .position(u8::is_ascii_whitespace)
        .unwrap_or(rest.len());
    let command = &rest[..end];
    BLOCKED_CLIENT_COMMANDS
        .iter()
        .find(|blocked| command.eq_ignore_ascii_case(blocked.as_bytes()))
        .map_or(FrameVerdict::Allow, |blocked| FrameVerdict::Block(blocked))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "http%3A%2F%2Fshiki-server%3A8080%2Fwopi%2Ffiles%2Fabc";

    #[test]
    fn blocks_script_commands_in_any_spelling() {
        for frame in [
            "executescript 1 (function(){}).apply(null, []);",
            "  executescript 1 x",
            "\texecutescript",
            "EXECUTESCRIPT 1 x",
            "ExecuteScript\n1",
            "proxyreturn 3 null",
        ] {
            assert!(
                matches!(
                    client_frame_verdict(frame.as_bytes()),
                    FrameVerdict::Block(_)
                ),
                "should block: {frame:?}"
            );
        }
    }

    #[test]
    fn allows_ordinary_editing_commands() {
        for frame in [
            "uno .uno:Bold",
            "key type=input char=97 key=0",
            "paste mimetype=text/html\n<p>executescript</p>",
            "load url=http%3A%2F%2Fx lang=ja",
            "executescriptx 1",
            "",
        ] {
            assert_eq!(
                client_frame_verdict(frame.as_bytes()),
                FrameVerdict::Allow,
                "should allow: {frame:?}"
            );
        }
    }

    #[test]
    fn document_socket_requires_upgrade_and_document_segment() {
        let ws = format!("/cool/{DOC}/ws");
        assert_eq!(route_for("GET", &ws, true), Some(Route::DocumentSocket));
        assert_eq!(route_for("GET", &ws, false), None);
        assert_eq!(
            route_for("GET", "/cool/ws", true),
            Some(Route::DocumentSocket)
        );
        assert_eq!(route_for("GET", "/cool/ws", false), None);
        assert_eq!(route_for("GET", "/cool/adminws/ws", true), None);
        // HTTP ロングポーリング版は検査を迂回できるので通さない。
        let proxy = format!("/cool/{DOC}/ws/sess/write/1");
        assert_eq!(route_for("POST", &proxy, false), None);
    }

    #[test]
    fn internal_only_endpoints_are_not_reachable() {
        for (method, path) in [
            ("POST", "/cool/convert-to/pdf"),
            ("POST", "/cool/convert-to"),
            ("POST", "/cool/extract-document-structure"),
            ("POST", "/cool/transform-document-structure"),
            ("POST", "/cool/extract-link-targets"),
            ("GET", "/cool/getMetrics"),
            ("GET", "/cool/adminws"),
            ("GET", "/browser/dist/admin/admin.html"),
            ("GET", "/browser/abc/admin/admin.html"),
            ("GET", "/lool/convert-to"),
            ("GET", "/cool/../hosting/discovery"),
            // パーセントエンコードした `..`（上流の URL を組み立てるときに `..` として畳まれる）。
            ("GET", "/browser/%2e%2e/cool/getMetrics"),
            ("GET", "/browser/.%2E/cool/getMetrics"),
            ("POST", "/browser/%2e%2e/cool/convert-to/pdf/x.html"),
            ("GET", "/browser/dist%2f..%2fadmin/admin.html"),
            ("GET", "/browser/%2e/dist/x.js"),
            // パーセントエンコードした管理画面（上流のファイル配信はデコードして配る）。
            ("GET", "/browser/dist/%61dmin/admin.html"),
            ("GET", "/browser/dist%2Fadmin%2Fadmin.html"),
            ("GET", "/browser//dist/x.js"),
            ("DELETE", "/browser/dist/bundle.js"),
            ("POST", "/hosting/discovery"),
        ] {
            assert_eq!(route_for(method, path, false), None, "{method} {path}");
        }
    }

    #[test]
    fn editor_assets_and_document_endpoints_pass() {
        for (method, path) in [
            ("GET", "/browser/dist/bundle.js"),
            ("POST", "/browser/de013a57f9/cool.html"),
            ("GET", "/hosting/discovery"),
            ("GET", "/hosting/capabilities"),
            ("GET", "/cool/clipboard"),
            ("POST", "/cool/clipboard"),
            ("GET", "/cool/media"),
        ] {
            assert_eq!(
                route_for(method, path, false),
                Some(Route::Http),
                "{method} {path}"
            );
        }
        assert_eq!(
            route_for("GET", &format!("/cool/{DOC}/download/abc123"), false),
            Some(Route::Http)
        );
        assert_eq!(
            route_for("POST", &format!("/cool/{DOC}/insertfile"), false),
            Some(Route::Http)
        );
        // 編集画面以外への POST は通さない。
        assert_eq!(route_for("POST", "/browser/dist/bundle.js", false), None);
    }
}
