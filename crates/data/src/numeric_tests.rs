#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::json;

use super::*;
use crate::model::{ExcessScale, FieldDef, FieldType, NumericSpec, RoundingMode};

fn d(s: &str) -> Dec {
    Dec::parse(s).unwrap_or_else(|| panic!("parse {s}"))
}

fn number_field(spec: NumericSpec) -> FieldDef {
    let mut f = FieldDef::new("amount", FieldType::Number);
    f.numeric_spec = Some(spec);
    f
}

fn spec(scale: u8, on: ExcessScale) -> NumericSpec {
    NumericSpec {
        scale,
        on_excess_scale: on,
        min: None,
        max: None,
    }
}

#[test]
fn parse_handles_plain_exponent_and_sign() {
    assert_eq!(d("12.50").canonical(), "12.5");
    assert_eq!(d("-0.000").canonical(), "0");
    assert_eq!(d("1e-7").canonical(), "0.0000001");
    assert_eq!(d("1.5E21").canonical(), "1500000000000000000000");
    assert_eq!(d("-3.25e1").canonical(), "-32.5");
    assert_eq!(d("007").canonical(), "7");
    for bad in ["", "-", "1.2.3", "abc", "1e", "1e999", "--1", "1 2"] {
        assert!(Dec::parse(bad).is_none(), "{bad} は不正");
    }
}

#[test]
fn ordering_is_numeric() {
    assert!(d("-2") < d("-1.5"));
    assert!(d("-1.5") < d("0"));
    assert!(d("0.09") < d("0.1"));
    assert!(d("9.99") < d("10"));
    assert_eq!(d("1.0").cmp(&d("1")), std::cmp::Ordering::Equal);
}

#[test]
fn rounding_modes_are_exact_in_decimal() {
    // 1.005 は二進では 1.00499.. だが、10 進の桁列で丸めるので HalfUp は 1.01。
    assert_eq!(
        d("1.005").round_to(2, RoundingMode::HalfUp).canonical(),
        "1.01"
    );
    assert_eq!(
        d("1.005").round_to(2, RoundingMode::HalfEven).canonical(),
        "1"
    );
    assert_eq!(
        d("1.015").round_to(2, RoundingMode::HalfEven).canonical(),
        "1.02"
    );
    assert_eq!(
        d("1.0051").round_to(2, RoundingMode::HalfEven).canonical(),
        "1.01"
    );
    assert_eq!(d("1.009").round_to(2, RoundingMode::Down).canonical(), "1");
    assert_eq!(d("1.001").round_to(2, RoundingMode::Up).canonical(), "1.01");
    // 負数は絶対値で丸める（HalfUp/Up は 0 から遠い方へ・Down は 0 へ）。
    assert_eq!(
        d("-2.5").round_to(0, RoundingMode::HalfUp).canonical(),
        "-3"
    );
    assert_eq!(
        d("-2.5").round_to(0, RoundingMode::HalfEven).canonical(),
        "-2"
    );
    assert_eq!(d("-2.9").round_to(0, RoundingMode::Down).canonical(), "-2");
    // 繰り上がりが整数部へ伝播する。
    assert_eq!(
        d("9.999").round_to(2, RoundingMode::HalfUp).canonical(),
        "10"
    );
    assert_eq!(d("0.7").round_to(0, RoundingMode::Up).canonical(), "1");
    // 超過桁が無ければそのまま。
    assert_eq!(d("1.2").round_to(3, RoundingMode::Up).canonical(), "1.2");
}

#[test]
fn reject_is_default_and_refuses_excess_scale() {
    let f = number_field(spec(2, ExcessScale::default()));
    assert_eq!(normalize_number(&f, &json!(12.34)).unwrap(), json!(12.34));
    assert_eq!(normalize_number(&f, &json!(12)).unwrap(), json!(12));
    let err = normalize_number(&f, &json!(12.345)).unwrap_err();
    assert_eq!(
        err.invalid_code(),
        Some(crate::InvalidCode::ValueOutOfRange),
        "{err:?}"
    );
}

#[test]
fn round_normalizes_before_range_check() {
    // 丸めで境界を跨ぐケース: max=10・HalfUp で 9.995 → 10.00 は範囲内（正規化の後に検証）。
    let mut s = spec(2, ExcessScale::Round(RoundingMode::HalfUp));
    s.max = Some(10.0);
    s.min = Some(0.0);
    let f = number_field(s);
    assert_eq!(normalize_number(&f, &json!(9.995)).unwrap(), json!(10));
    assert!(
        normalize_number(&f, &json!(10.005)).is_err(),
        "10.01 は範囲外"
    );
    assert!(
        normalize_number(&f, &json!(-0.001)).is_ok(),
        "-0.00 → 0 は範囲内"
    );
    assert!(normalize_number(&f, &json!(-0.01)).is_err());
}

#[test]
fn values_f64_cannot_hold_are_rejected() {
    // 正規化後の値が f64 の最短表記で同じ値へ戻らないなら、本体と索引がずれるので拒否する。
    // u64 の最大値は i64 にも f64 にも正確に載らない。
    let f = number_field(spec(0, ExcessScale::Round(RoundingMode::HalfUp)));
    assert!(normalize_number(&f, &json!(u64::MAX)).is_err());
    // i64 に載る整数・f64 で表せる小数は通る。
    assert_eq!(
        normalize_number(&f, &json!(9_007_199_254_740_993_i64)).unwrap(),
        json!(9_007_199_254_740_993_i64)
    );
    let f9 = number_field(spec(9, ExcessScale::Round(RoundingMode::HalfUp)));
    assert_eq!(normalize_number(&f9, &json!(0.1)).unwrap(), json!(0.1));
}

#[test]
fn without_spec_value_is_kept_as_is() {
    let f = FieldDef::new("n", FieldType::Number);
    assert_eq!(
        normalize_number(&f, &json!(1.23456)).unwrap(),
        json!(1.23456)
    );
}

#[test]
fn spec_validation() {
    let mut text = FieldDef::new("t", FieldType::Text);
    text.numeric_spec = Some(spec(2, ExcessScale::Reject));
    assert!(validate_spec(&text).is_err(), "number 以外は不可");
    assert!(validate_spec(&number_field(spec(10, ExcessScale::Reject))).is_err());
    let mut inverted = spec(2, ExcessScale::Reject);
    inverted.min = Some(5.0);
    inverted.max = Some(1.0);
    assert!(validate_spec(&number_field(inverted)).is_err(), "min > max");
    let mut nan = spec(2, ExcessScale::Reject);
    nan.min = Some(f64::NAN);
    assert!(validate_spec(&number_field(nan)).is_err());
    assert!(validate_spec(&number_field(spec(9, ExcessScale::Reject))).is_ok());
}

#[test]
fn canonical_number_unifies_representations() {
    let a: serde_json::Number = serde_json::from_str("1").unwrap();
    let b: serde_json::Number = serde_json::from_str("1.0").unwrap();
    let c: serde_json::Number = serde_json::from_str("1e0").unwrap();
    assert_eq!(canonical_number(&a), canonical_number(&b));
    assert_eq!(canonical_number(&a), canonical_number(&c));
    assert_eq!(canonical_number(&a).unwrap(), "1");
}

#[test]
fn spec_serde_shape() {
    let s: NumericSpec = serde_json::from_value(json!({
        "scale": 2, "on_excess_scale": { "round": "half_even" }, "min": 0
    }))
    .unwrap();
    assert_eq!(
        s.on_excess_scale,
        ExcessScale::Round(RoundingMode::HalfEven)
    );
    assert_eq!(s.min, Some(0.0));
    let r: NumericSpec = serde_json::from_value(json!({ "scale": 0 })).unwrap();
    assert_eq!(r.on_excess_scale, ExcessScale::Reject, "既定は reject");
    assert_eq!(
        serde_json::to_value(&r).unwrap(),
        json!({ "scale": 0, "on_excess_scale": "reject" })
    );
}
