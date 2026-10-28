//! 正確な 10 進演算（数式・ロールアップの `numeric` 精度・data-platform §7.3）。
//!
//! 金額の四則演算・合計を浮動小数で行うと `0.1 + 0.2 = 0.30000000000000004` になる。ここでは
//! **仮数（i128）× 10^-scale** の固定小数で計算し、二進誤差を持ち込まない。入力の JSON 数値は最短の
//! 10 進表記で解釈する（[`crate::numeric::Dec`] と同じ規則: `0.1` は 0.1 ちょうど）。
//!
//! - 加減乗は正確（桁あふれ＝ 38 桁超は `None` → 呼び出し側で値なし）。
//! - 除算は「被演算子の小数桁の大きい方 + [`DIV_EXTRA_SCALE`]」桁へ偶数丸め（銀行丸め）。
//! - `round` は 0 から遠い方への四捨五入（日本の会計の四捨五入）。
//! - JSON へ戻すときは、f64 の最短表記で同じ 10 進値へ戻る場合だけ数値、戻らない（有効桁が多すぎる）
//!   場合は 10 進表記の文字列にする（精度を黙って落とさない）。

use std::cmp::Ordering;

use serde_json::{Number, Value};

use crate::numeric::Dec;

/// 小数桁の上限。
pub(crate) const MAX_SCALE: u32 = 28;
/// 除算で被演算子の小数桁に足す桁数。
pub(crate) const DIV_EXTRA_SCALE: u32 = 6;

/// 固定小数（`mant × 10^-scale`）。常に末尾ゼロを落とした正規形で持つ（`1.50` は `15, 1`）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Decimal {
    mant: i128,
    scale: u32,
}

impl PartialEq for Decimal {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Decimal {}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> Ordering {
        // 同じ小数桁へ揃えて比べる（揃えられないほど大きい差は符号と桁で決まる）。
        match align(*self, *other) {
            Some((a, b, _)) => a.cmp(&b),
            None => {
                // 桁あふれ: 小数桁の小さい方を拡大できない＝整数部が巨大。f64 近似で比べる。
                self.to_f64()
                    .partial_cmp(&other.to_f64())
                    .unwrap_or(Ordering::Equal)
            }
        }
    }
}

fn pow10(n: u32) -> Option<i128> {
    10i128.checked_pow(n)
}

/// 2 数を同じ小数桁へ揃える（揃えた仮数と小数桁）。
fn align(a: Decimal, b: Decimal) -> Option<(i128, i128, u32)> {
    let s = a.scale.max(b.scale);
    let am = a.mant.checked_mul(pow10(s - a.scale)?)?;
    let bm = b.mant.checked_mul(pow10(s - b.scale)?)?;
    Some((am, bm, s))
}

impl Decimal {
    pub(crate) const ZERO: Decimal = Decimal { mant: 0, scale: 0 };

    fn new(mant: i128, scale: u32) -> Decimal {
        let (mut mant, mut scale) = (mant, scale);
        while scale > 0 && mant % 10 == 0 {
            mant /= 10;
            scale -= 1;
        }
        Decimal { mant, scale }
    }

    pub(crate) fn from_i64(n: i64) -> Decimal {
        Decimal::new(i128::from(n), 0)
    }

    /// 10 進表記を読む（`1e-7`・`-12.50` など）。38 桁を超える・小数桁が上限超過なら `None`。
    pub(crate) fn parse(s: &str) -> Option<Decimal> {
        let canonical = Dec::parse(s)?.canonical();
        let (neg, body) = match canonical.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, canonical.as_str()),
        };
        let (ip, fp) = body.split_once('.').unwrap_or((body, ""));
        let scale = u32::try_from(fp.len()).ok()?;
        if scale > MAX_SCALE {
            return None;
        }
        let digits = format!("{ip}{fp}");
        let mant: i128 = digits.parse().ok()?;
        Some(Decimal::new(if neg { -mant } else { mant }, scale))
    }

    /// JSON 数値を読む（最短 10 進表記として解釈）。
    pub(crate) fn from_number(n: &Number) -> Option<Decimal> {
        Decimal::parse(&n.to_string())
    }

    /// 整数として取り出す（小数部があるか範囲外なら `None`）。
    pub(crate) fn to_i64(self) -> Option<i64> {
        if self.scale != 0 {
            return None;
        }
        i64::try_from(self.mant).ok()
    }

    pub(crate) fn checked_add(self, o: Decimal) -> Option<Decimal> {
        let (a, b, s) = align(self, o)?;
        Some(Decimal::new(a.checked_add(b)?, s))
    }

    pub(crate) fn checked_sub(self, o: Decimal) -> Option<Decimal> {
        let (a, b, s) = align(self, o)?;
        Some(Decimal::new(a.checked_sub(b)?, s))
    }

    pub(crate) fn checked_mul(self, o: Decimal) -> Option<Decimal> {
        let mant = self.mant.checked_mul(o.mant)?;
        let scale = self.scale + o.scale;
        if scale > MAX_SCALE {
            return Some(Decimal::new(mant, scale).round_half_even(MAX_SCALE));
        }
        Some(Decimal::new(mant, scale))
    }

    /// 除算（0 除算は `None`）。結果は `max(小数桁) + DIV_EXTRA_SCALE` 桁へ偶数丸め。
    pub(crate) fn checked_div(self, o: Decimal) -> Option<Decimal> {
        if o.mant == 0 {
            return None;
        }
        let target = (self.scale.max(o.scale) + DIV_EXTRA_SCALE).min(MAX_SCALE);
        // self / o = (a·10^-sa) / (b·10^-sb)。商を 10^-target 単位で求める:
        // q = a · 10^(target + sb - sa) / b。
        let shift = target + o.scale;
        let num = if shift >= self.scale {
            self.mant.checked_mul(pow10(shift - self.scale)?)?
        } else {
            self.mant / pow10(self.scale - shift)?
        };
        let q = num / o.mant;
        let r = num % o.mant;
        let q = round_quotient(q, r, o.mant, RoundMode::HalfEven)?;
        Some(Decimal::new(q, target))
    }

    /// `scale` 桁へ 0 から遠い方へ四捨五入する（`round` 関数）。
    pub(crate) fn round_half_up(self, scale: u32) -> Decimal {
        self.round_with(scale, RoundMode::HalfUp)
    }

    fn round_half_even(self, scale: u32) -> Decimal {
        self.round_with(scale, RoundMode::HalfEven)
    }

    fn round_with(self, scale: u32, mode: RoundMode) -> Decimal {
        if self.scale <= scale {
            return self;
        }
        let Some(div) = pow10(self.scale - scale) else {
            return Decimal::ZERO;
        };
        let q = self.mant / div;
        let r = self.mant % div;
        round_quotient(q, r, div, mode).map_or(self, |q| Decimal::new(q, scale))
    }

    /// 末尾ゼロなしの 10 進表記（`-12.5` / `0` / `120`）。
    pub(crate) fn canonical(self) -> String {
        let neg = self.mant < 0;
        let digits = self.mant.unsigned_abs().to_string();
        let scale = self.scale as usize;
        let body = if scale == 0 {
            digits
        } else if digits.len() > scale {
            let (i, f) = digits.split_at(digits.len() - scale);
            format!("{i}.{f}")
        } else {
            format!("0.{}{digits}", "0".repeat(scale - digits.len()))
        };
        if neg {
            format!("-{body}")
        } else {
            body
        }
    }

    fn to_f64(self) -> f64 {
        self.canonical().parse().unwrap_or(f64::NAN)
    }

    /// JSON へ戻す: 整数（i64 範囲）は整数、f64 の最短表記で同じ値へ戻るなら数値、それ以外は文字列。
    pub(crate) fn to_json(self) -> Value {
        if self.scale == 0 {
            if let Ok(i) = i64::try_from(self.mant) {
                return Value::Number(i.into());
            }
        }
        let s = self.canonical();
        let exact = s
            .parse::<f64>()
            .ok()
            .filter(|f| f.is_finite())
            .filter(|f| Dec::from_f64(*f).is_some_and(|d| Some(d) == Dec::parse(&s)));
        match exact.and_then(Number::from_f64) {
            Some(n) => Value::Number(n),
            None => Value::String(s),
        }
    }
}

#[derive(Clone, Copy)]
enum RoundMode {
    HalfUp,
    HalfEven,
}

/// 商 `q`・余り `r`（除数 `d`）を丸める。
fn round_quotient(q: i128, r: i128, d: i128, mode: RoundMode) -> Option<i128> {
    if r == 0 {
        return Some(q);
    }
    let twice = r.unsigned_abs().checked_mul(2)?;
    let dabs = d.unsigned_abs();
    let away = match twice.cmp(&dabs) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => match mode {
            RoundMode::HalfUp => true,
            RoundMode::HalfEven => q % 2 != 0,
        },
    };
    if !away {
        return Some(q);
    }
    // 真の商の符号（r と d の符号から）に向けて 1 動かす。
    let negative = (r < 0) != (d < 0);
    if negative {
        q.checked_sub(1)
    } else {
        q.checked_add(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn d(s: &str) -> Decimal {
        Decimal::parse(s).unwrap()
    }

    #[test]
    fn money_arithmetic_is_exact() {
        // f64 なら 0.30000000000000004。
        assert_eq!(d("0.1").checked_add(d("0.2")).unwrap().canonical(), "0.3");
        assert_eq!(d("1.1").checked_mul(d("3")).unwrap().canonical(), "3.3");
        assert_eq!(
            d("100").checked_sub(d("0.01")).unwrap().canonical(),
            "99.99"
        );
        assert_eq!(
            d("19.99").checked_mul(d("3")).unwrap().to_json(),
            json!(59.97)
        );
        let mut sum = Decimal::ZERO;
        for _ in 0..10 {
            sum = sum.checked_add(d("0.1")).unwrap();
        }
        assert_eq!(sum.to_json(), json!(1));
    }

    #[test]
    fn division_rounds_half_even_at_fixed_scale() {
        assert_eq!(d("1").checked_div(d("3")).unwrap().canonical(), "0.333333");
        assert_eq!(d("2").checked_div(d("3")).unwrap().canonical(), "0.666667");
        assert_eq!(d("10").checked_div(d("4")).unwrap().canonical(), "2.5");
        assert_eq!(
            d("-1").checked_div(d("3")).unwrap().canonical(),
            "-0.333333"
        );
        assert_eq!(
            d("1000000").checked_div(d("3")).unwrap().canonical(),
            "333333.333333"
        );
        assert!(d("1").checked_div(Decimal::ZERO).is_none());
        // 偶数丸め: 1/16 = 0.0625（4 桁）は 0 + 6 = 6 桁に収まるので正確。
        assert_eq!(d("1").checked_div(d("16")).unwrap().canonical(), "0.0625");
        // 6 桁へ丸める: 0.000000125 → 0、0.000000625 → 0.000001。
        assert_eq!(d("1").checked_div(d("8000000")).unwrap().canonical(), "0");
        assert_eq!(d("3").checked_div(d("8000000")).unwrap().canonical(), "0");
        assert_eq!(
            d("5").checked_div(d("8000000")).unwrap().canonical(),
            "0.000001"
        );
        // ちょうど半分は偶数へ: 0.0000005 → 0、0.0000015 → 0.000002。
        assert_eq!(d("1").checked_div(d("2000000")).unwrap().canonical(), "0");
        assert_eq!(
            d("3").checked_div(d("2000000")).unwrap().canonical(),
            "0.000002"
        );
    }

    #[test]
    fn round_half_up_is_away_from_zero() {
        assert_eq!(d("2.345").round_half_up(2).canonical(), "2.35");
        assert_eq!(d("-2.345").round_half_up(2).canonical(), "-2.35");
        assert_eq!(d("2.344").round_half_up(2).canonical(), "2.34");
        assert_eq!(d("12").round_half_up(0).canonical(), "12");
    }

    #[test]
    fn comparison_and_parse() {
        assert_eq!(d("1.0"), d("1"));
        assert!(d("-0.5") < d("0.1"));
        assert!(d("10") > d("9.999"));
        assert_eq!(d("1e3").canonical(), "1000");
        assert_eq!(d("1.5E-2").canonical(), "0.015");
        assert!(Decimal::parse("abc").is_none());
    }

    #[test]
    fn overflow_is_none_not_wrong() {
        let big = d("99999999999999999999999999999999999999");
        assert!(big.checked_add(big).is_none());
        assert!(big.checked_mul(big).is_none());
    }

    #[test]
    fn json_falls_back_to_string_when_f64_cannot_hold_it() {
        assert_eq!(d("3").to_json(), json!(3));
        assert_eq!(d("0.3").to_json(), json!(0.3));
        let s = d("12345678901234567890.123456");
        assert_eq!(s.to_json(), json!("12345678901234567890.123456"));
    }
}
