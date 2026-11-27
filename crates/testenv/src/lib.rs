//! 結合テストの外部依存（Postgres・OpenFGA・Redis・Qdrant・実 LLM 等）の接続先を読む。
//!
//! 結合テストは、接続先の環境変数が無いと「スキップ」して **pass に数えられる**。ローカルで
//! compose を立てずに `cargo test` を回せるようにするための作りだが、CI で env を渡し忘れたり
//! 変数名を誤ったりしても緑のまま、意図したテストが一度も走らない事態を検知できなかった。
//!
//! そこで、結合テストの門番になる環境変数は [`var`] で読む。[`var`] は `std::env::var` と同じ型を
//! 返すので、呼び出し側のスキップ処理（`let Ok(url) = … else { return; }` 等）はそのまま使える。
//! 違いは、`SHIKI_REQUIRE_IT=1` のときに未設定を **panic（＝テスト失敗）** にすること。
//!
//! - `SHIKI_REQUIRE_IT=1`: CI の結合テストジョブ（coverage・data-strategies）が立てる。
//! - `SHIKI_REQUIRE_IT_EXCEPT=A,B`: CI が用意しない依存（実 LLM・Office エンジン・VM 系サンドボックス等）。
//!   ここに挙げた変数は、未設定でも従来どおりスキップする。**このリストが「CI で一度も走らない
//!   結合テスト」の一覧そのもの**なので、ci.yml の 1 か所で管理し Job Summary に出す。
//!
//! 結合テストの門番になる変数を `std::env::var` で直接読むことは `scripts/check-it-env.sh`
//! （CI の quality ジョブ）が禁じる。

use std::env::VarError;

/// 未設定を失敗にするかどうかを決める環境変数（`1` で有効）。
pub const REQUIRE_ENV: &str = "SHIKI_REQUIRE_IT";
/// 未設定でもスキップを許す変数のカンマ区切り一覧。
pub const EXCEPT_ENV: &str = "SHIKI_REQUIRE_IT_EXCEPT";

/// 結合テストの門番になる環境変数を読む（`std::env::var` の置き換え）。
///
/// 値があればそれを返す。未設定なら `Err` を返し、呼び出し側はスキップする。
/// ただし `SHIKI_REQUIRE_IT=1` で、かつ `SHIKI_REQUIRE_IT_EXCEPT` に挙がっていなければ panic する。
///
/// # Errors
///
/// 変数が未設定、または UTF-8 でないとき（`std::env::var` と同じ）。
///
/// # Panics
///
/// `SHIKI_REQUIRE_IT=1` の下で、許可リストに無い変数が未設定のとき。
pub fn var(name: &str) -> Result<String, VarError> {
    let value = std::env::var(name);
    let require = std::env::var(REQUIRE_ENV).ok();
    let except = std::env::var(EXCEPT_ENV).ok();
    if let Some(message) = missing_message(name, &value, require.as_deref(), except.as_deref()) {
        fail(&message);
    }
    value
}

/// 未設定を失敗にすべきなら、その理由を返す（判定だけを切り出した純関数）。
fn missing_message(
    name: &str,
    value: &Result<String, VarError>,
    require: Option<&str>,
    except: Option<&str>,
) -> Option<String> {
    if value.is_ok() || require != Some("1") {
        return None;
    }
    if except.is_some_and(|list| list.split(',').map(str::trim).any(|e| e == name)) {
        return None;
    }
    Some(format!(
        "{REQUIRE_ENV}=1 の下で結合テストの接続先 `{name}` が未設定です。\
         このジョブでテストが黙ってスキップされるのを防ぐため失敗にしています。\
         CI のジョブに `{name}` を渡すか、CI で用意しない依存なら {EXCEPT_ENV} に足してください。"
    ))
}

/// 接続先の環境変数ではなく「ビルド済みバイナリ」などが無くてスキップするときに呼ぶ。
///
/// `name` はその依存を指す環境変数名（例 `SHIKI_TABULAR_RUNNER`）。[`var`] と同じく、
/// `SHIKI_REQUIRE_IT=1` で `SHIKI_REQUIRE_IT_EXCEPT` に無ければ panic し、それ以外はスキップを告げる。
///
/// # Panics
///
/// `SHIKI_REQUIRE_IT=1` の下で、許可リストに無い依存が見つからないとき。
#[allow(clippy::print_stderr)]
pub fn missing(name: &str, what: &str) {
    let require = std::env::var(REQUIRE_ENV).ok();
    let except = std::env::var(EXCEPT_ENV).ok();
    let unset: Result<String, VarError> = Err(VarError::NotPresent);
    if let Some(message) = missing_message(name, &unset, require.as_deref(), except.as_deref()) {
        fail(&format!("{what}: {message}"));
    }
    eprintln!("{what}のためスキップ（{name}）");
}

// 結合テストを「失敗」にする唯一の手段が panic なので、ここだけ許す。
#[allow(clippy::panic)]
fn fail(message: &str) -> ! {
    panic!("{message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: &str = "OPENFGA_TEST_URL";

    fn unset() -> Result<String, VarError> {
        Err(VarError::NotPresent)
    }

    #[test]
    fn skips_when_not_required() {
        assert_eq!(missing_message(NAME, &unset(), None, None), None);
        assert_eq!(missing_message(NAME, &unset(), Some("0"), None), None);
    }

    #[test]
    fn passes_when_set_even_if_required() {
        let set = Ok("http://localhost:8082".to_string());
        assert_eq!(missing_message(NAME, &set, Some("1"), None), None);
    }

    #[test]
    fn fails_when_required_and_unset() {
        let message = missing_message(NAME, &unset(), Some("1"), Some("KEYCLOAK_TEST_URL"));
        assert!(message.is_some_and(|m| m.contains(NAME)));
    }

    #[test]
    fn except_list_allows_skip() {
        let except = Some("KEYCLOAK_TEST_URL, OPENFGA_TEST_URL");
        assert_eq!(missing_message(NAME, &unset(), Some("1"), except), None);
    }

    #[test]
    fn except_list_matches_whole_names_only() {
        // 前方一致で他の変数まで許してしまわない。
        let except = Some("OPENFGA_TEST");
        assert!(missing_message(NAME, &unset(), Some("1"), except).is_some());
    }

    #[test]
    fn missing_skips_when_not_required() {
        // 既定（SHIKI_REQUIRE_IT 未設定）では panic せずにスキップを告げるだけ。
        if std::env::var(REQUIRE_ENV).as_deref() != Ok("1") {
            missing("SHIKI_TESTENV_PROBE", "テスト用の依存が無い");
        }
    }

    #[test]
    fn var_returns_value_when_set() {
        // CARGO_PKG_NAME は cargo が必ず渡す。
        assert_eq!(var("CARGO_PKG_NAME").as_deref(), Ok("shiki-testenv"));
    }
}
