//! `Shiki.*` の追加 API をゲスト wasm を再ビルドせずに足す前置き JS（Task 9.12 の方式・Task 15.7 で共通化）。
//!
//! ゲストのブートストラップが公開する低レベル import `__shiki_hostcall` を直接使い、`Shiki.data.*`
//! （と面によっては `Shiki.notify.*`）を定義する。api 名はホスト側 [`crate::host::ALLOWED_APIS`] の閉集合に
//! 照合され、能力の認可・スコープ天井・監査はホスト側のハンドラ（ワークフローの能力ゲートウェイ／
//! ミニアプリのゲートウェイ）が担う。ここは糖衣だけで権限は増えない。
//!
//! **コンパイル済みユーザーコードの前**に連結する（`compile` の禁止構文 lint はユーザーコードにだけ効く）。
//! 引数の形（`table_id` / `record_id` / `body`）はミニアプリのゲートウェイ委譲とワークフローの
//! `HostBridge` で共通（同じ script が両方の面で同じ意味で動く）。

/// 前置き JS の先頭（ホスト呼び出しの糖衣 `call` を閉包に持つ）。
const HEAD: &str = r"
(function () {
  function call(api, args) {
    var resp = __shiki_hostcall(JSON.stringify({ api: api, args: args || {} }));
    var parsed = JSON.parse(resp);
    if (parsed && parsed.ok) { return parsed.value; }
    var err = (parsed && parsed.error) || {};
    var e = new Error(err.message || 'host call failed');
    e.name = 'ShikiError';
    e.code = err.code || 'internal';
    e.retryable = !!err.retryable;
    throw e;
  }
";

/// `Shiki.data.*`（query は data の `DataQuery` をそのまま渡す・面ごとの独自フィルタ表現を持たない）。
const DATA: &str = r"
  Shiki.data = {
    listTables: function () { return call('data.list_tables', {}); },
    query: function (tableId, body) { return call('data.query', { table_id: tableId, body: body || {} }); },
    get: function (tableId, recordId) { return call('data.get', { table_id: tableId, record_id: recordId }); },
    create: function (tableId, data) { return call('data.create', { table_id: tableId, body: { data: data } }); },
    update: function (tableId, recordId, patch, expectedRev) {
      return call('data.update', { table_id: tableId, record_id: recordId, body: { patch: patch, expected_rev: expectedRev } });
    },
    transition: function (tableId, recordId, to, expectedRev) {
      return call('data.transition', { table_id: tableId, record_id: recordId, body: { to: to, expected_rev: expectedRev } });
    }
  };
";

/// `Shiki.notify.*`（ミニアプリ関数のみ。ワークフローの script は通知ノードを使う）。
const NOTIFY: &str = r"
  Shiki.notify = {
    send: function (recipient, title, body) {
      return call('notify.send', { body: { recipient: recipient, title: title, body: body || null } });
    }
  };
";

const TAIL: &str = r"
})();
";

/// ワークフロー／skill の script（`HostBridge`）向けの前置き（`Shiki.data.*`）。
#[must_use]
pub fn workflow_prelude() -> String {
    [HEAD, DATA, TAIL].concat()
}

/// ミニアプリ関数（B2・ゲートウェイ委譲）向けの前置き（`Shiki.data.*` ＋ `Shiki.notify.*`）。
#[must_use]
pub fn miniapp_prelude() -> String {
    [HEAD, DATA, NOTIFY, TAIL].concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::is_allowed_api;

    /// 前置きが呼ぶ api 名は全て閉集合に属する（閉集合外はフレーム検証で実行ごと破棄される）。
    #[test]
    fn prelude_apis_are_in_the_closed_set() {
        for p in [workflow_prelude(), miniapp_prelude()] {
            for (i, _) in p.match_indices("call('") {
                let rest = &p[i + 6..];
                let api = &rest[..rest.find('\'').unwrap_or(0)];
                assert!(is_allowed_api(api), "閉集合外の api: {api}");
            }
        }
        assert!(workflow_prelude().contains("transition:"));
        assert!(!workflow_prelude().contains("Shiki.notify"));
        assert!(miniapp_prelude().contains("Shiki.notify"));
    }
}
