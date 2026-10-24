//! `DataConfig` の読み込みのユニットテスト。

use super::*;

/// compose の `${VAR:-}` は未設定の変数を空文字で渡す。カーソル鍵ファイル・差分配信の設定が空文字なら
/// 未設定と同じに読む（空のパスを開こうとして・空の数値の解釈に失敗して起動を落とさない）。
#[test]
fn data_settings_treat_empty_strings_as_unset() {
    let read = |pairs: &[(&str, &str)]| -> DataConfig {
        let mut f = figment::Figment::new();
        for (k, v) in pairs {
            f = f.merge((*k, *v));
        }
        f.extract().unwrap()
    };
    let empty = read(&[
        ("cursor_key_file", ""),
        ("stream.authz_ttl_ms", ""),
        ("stream.retention_secs", " "),
    ]);
    assert_eq!(empty.cursor_key_file, None);
    assert_eq!(empty.stream.authz_ttl_ms, None);
    assert_eq!(empty.stream.retention_secs, None);

    let set = read(&[
        ("cursor_key_file", "/run/secrets/k"),
        ("stream.authz_ttl_ms", "2000"),
    ]);
    assert_eq!(set.cursor_key_file.as_deref(), Some("/run/secrets/k"));
    assert_eq!(set.stream.authz_ttl_ms, Some(2000));
    let num: DataConfig = figment::Figment::new()
        .merge(("stream.retention_secs", 900_u64))
        .extract()
        .unwrap();
    assert_eq!(num.stream.retention_secs, Some(900));
    assert!(figment::Figment::new()
        .merge(("stream.retention_secs", "ten"))
        .extract::<DataConfig>()
        .is_err());
}
