//! `number` 型の精度宣言（[`NumericSpec`]）による正規化（data-platform §4.2・Task 15.2）。
//!
//! 規則（固定・曖昧さを残さない）:
//! 1. 書込時に値を `scale` 桁へ正規化する。`on_excess_scale = reject`（既定）なら超過桁のある入力は
//!    422、`round(mode)` なら宣言のモードで丸める
//! 2. `min` / `max` は**正規化の後**に検証する（丸めで境界を跨ぐ場合の挙動を一意にする）
//! 3. 本体・索引・集計はすべて正規化後の同一値を使う。本体の JSON 数値（f64）へ戻せない桁数
//!    （有効桁が多すぎる）は 422 にして、本体と索引で値がずれる余地を残さない
//!
//! 浮動小数の二進誤差を持ち込まないよう、演算は**10 進の桁列**で行う（入力の JSON 数値は最短の
//! 10 進表記で解釈する: `0.1` は 0.1 ちょうど）。

use std::cmp::Ordering;

use serde_json::{Number, Value};

use crate::model::{ExcessScale, FieldDef, FieldType, NumericSpec, RoundingMode};
use crate::DataError;

/// `scale` の上限（data-platform §4.2）。
pub(crate) const MAX_SCALE: u8 = 9;
/// 受け付ける 10 進指数の絶対値の上限（f64 の範囲 ≒ 1e308 を十分に覆う）。
const MAX_EXPONENT: i64 = 400;

/// 10 進の有限小数（符号・整数部・小数部）。整数部は先頭ゼロなし、小数部は末尾ゼロなし。
/// ゼロは `neg = false`・両方空。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dec {
    neg: bool,
    int: String,
    frac: String,
}

impl Dec {
    /// 10 進表記（`-12.5`・`1e-7`・`1.5E21`）を読む。不正な形は `None`。
    pub(crate) fn parse(s: &str) -> Option<Dec> {
        let s = s.trim();
        let (neg, body) = match s.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, s.strip_prefix('+').unwrap_or(s)),
        };
        let (mantissa, exp) = match body.find(['e', 'E']) {
            Some(pos) => {
                let e: i64 = body[pos + 1..].parse().ok()?;
                (&body[..pos], e)
            }
            None => (body, 0),
        };
        if exp.abs() > MAX_EXPONENT {
            return None;
        }
        let (ip, fp) = match mantissa.split_once('.') {
            Some((a, b)) => (a, b),
            None => (mantissa, ""),
        };
        if (ip.is_empty() && fp.is_empty())
            || !ip.bytes().all(|b| b.is_ascii_digit())
            || !fp.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let digits = format!("{ip}{fp}");
        // 小数点の位置（digits の先頭から何桁目の後ろか）。
        let point = i64::try_from(ip.len()).ok()? + exp;
        let len = i64::try_from(digits.len()).ok()?;
        let (int, frac) = if point <= 0 {
            let zeros = usize::try_from(-point).ok()?;
            (String::new(), format!("{}{digits}", "0".repeat(zeros)))
        } else if point >= len {
            let zeros = usize::try_from(point - len).ok()?;
            (format!("{digits}{}", "0".repeat(zeros)), String::new())
        } else {
            let p = usize::try_from(point).ok()?;
            (digits[..p].to_string(), digits[p..].to_string())
        };
        Some(Dec::from_parts(neg, &int, &frac))
    }

    /// f64 を最短の 10 進表記として読む（有限値のみ）。
    pub(crate) fn from_f64(f: f64) -> Option<Dec> {
        if !f.is_finite() {
            return None;
        }
        Dec::parse(&format!("{f}"))
    }

    fn from_parts(neg: bool, int: &str, frac: &str) -> Dec {
        let int = int.trim_start_matches('0').to_string();
        let frac = frac.trim_end_matches('0').to_string();
        let zero = int.is_empty() && frac.is_empty();
        Dec {
            neg: neg && !zero,
            int,
            frac,
        }
    }

    /// 小数部の桁数（末尾ゼロを除く）。
    pub(crate) fn scale(&self) -> usize {
        self.frac.len()
    }

    /// `scale` 桁へ丸める（超過桁が無ければそのまま）。
    pub(crate) fn round_to(&self, scale: usize, mode: RoundingMode) -> Dec {
        if self.frac.len() <= scale {
            return self.clone();
        }
        let kept = &self.frac[..scale];
        let rest = &self.frac[scale..];
        // rest は末尾ゼロを除いてあるので必ず非ゼロ。
        let first = rest.as_bytes()[0];
        let beyond_half = rest[1..].bytes().any(|b| b != b'0');
        let last_kept_odd = kept
            .bytes()
            .last()
            .or_else(|| self.int.bytes().last())
            .is_some_and(|b| (b - b'0') % 2 == 1);
        let bump = match mode {
            RoundingMode::Down => false,
            RoundingMode::Up => true,
            RoundingMode::HalfUp => first >= b'5',
            RoundingMode::HalfEven => match first.cmp(&b'5') {
                Ordering::Greater => true,
                Ordering::Less => false,
                Ordering::Equal => beyond_half || last_kept_odd,
            },
        };
        if !bump {
            return Dec::from_parts(self.neg, &self.int, kept);
        }
        // 絶対値に 1ulp（10^-scale）を足す: 整数部＋保持桁を 1 つの整数とみなして +1。
        let mut digits: Vec<u8> = format!("{}{kept}", self.int).into_bytes();
        let mut carry = true;
        for d in digits.iter_mut().rev() {
            if !carry {
                break;
            }
            if *d == b'9' {
                *d = b'0';
            } else {
                *d += 1;
                carry = false;
            }
        }
        if carry {
            digits.insert(0, b'1');
        }
        let s = String::from_utf8(digits).unwrap_or_default();
        let split = s.len() - kept.len();
        Dec::from_parts(self.neg, &s[..split], &s[split..])
    }

    /// 末尾ゼロなしの正準表記（`-12.5` / `0` / `120`）。unique の正規化キーに使う。
    pub(crate) fn canonical(&self) -> String {
        let int = if self.int.is_empty() { "0" } else { &self.int };
        let sign = if self.neg { "-" } else { "" };
        if self.frac.is_empty() {
            format!("{sign}{int}")
        } else {
            format!("{sign}{int}.{}", self.frac)
        }
    }

    fn cmp_abs(&self, other: &Dec) -> Ordering {
        self.int
            .len()
            .cmp(&other.int.len())
            .then_with(|| self.int.cmp(&other.int))
            .then_with(|| {
                let w = self.frac.len().max(other.frac.len());
                let a = format!("{:0<w$}", self.frac);
                let b = format!("{:0<w$}", other.frac);
                a.cmp(&b)
            })
    }
}

impl PartialOrd for Dec {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Dec {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.neg, other.neg) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => self.cmp_abs(other),
            (true, true) => other.cmp_abs(self),
        }
    }
}

/// JSON 数値の 10 進正準表記（`1` と `1.0` を同じキーにする・unique の正規化用）。
pub(crate) fn canonical_number(n: &Number) -> Option<String> {
    Dec::parse(&n.to_string()).map(|d| d.canonical())
}

/// `NumericSpec` 自体の妥当性（スキーマ検証）。
pub(crate) fn validate_spec(f: &FieldDef) -> Result<(), DataError> {
    let Some(spec) = &f.numeric_spec else {
        return Ok(());
    };
    let name = &f.key;
    if f.field_type != FieldType::Number {
        return Err(DataError::Invalid(format!(
            "'{name}': numeric_spec は number 型にのみ指定できます"
        )));
    }
    if spec.scale > MAX_SCALE {
        return Err(DataError::Invalid(format!(
            "'{name}': numeric_spec.scale は 0〜{MAX_SCALE} で指定してください"
        )));
    }
    let bound = |v: Option<f64>, label: &str| -> Result<Option<Dec>, DataError> {
        v.map(|x| {
            Dec::from_f64(x).ok_or_else(|| {
                DataError::Invalid(format!(
                    "'{name}': numeric_spec.{label} は有限の数値で指定してください"
                ))
            })
        })
        .transpose()
    };
    let (min, max) = (bound(spec.min, "min")?, bound(spec.max, "max")?);
    if let (Some(lo), Some(hi)) = (&min, &max) {
        if lo > hi {
            return Err(DataError::Invalid(format!(
                "'{name}': numeric_spec.min が max より大きくなっています"
            )));
        }
    }
    Ok(())
}

/// number 値を宣言に従って正規化する（書込検証から呼ぶ・spec なしは入力のまま）。
pub(crate) fn normalize_number(f: &FieldDef, v: &Value) -> Result<Value, DataError> {
    let name = &f.key;
    let Some(spec) = &f.numeric_spec else {
        return Ok(v.clone());
    };
    let Value::Number(n) = v else {
        return Err(DataError::Invalid(format!(
            "フィールド '{name}' は 数値 である必要があります"
        )));
    };
    let dec = Dec::parse(&n.to_string())
        .ok_or_else(|| DataError::Invalid(format!("'{name}' の数値を解釈できません")))?;
    let scale = usize::from(spec.scale);
    let normalized = if dec.scale() > scale {
        match spec.on_excess_scale {
            ExcessScale::Reject => {
                return Err(DataError::Invalid(format!(
                    "'{name}' は小数 {scale} 桁までです（超過桁は丸めずに拒否します）"
                )));
            }
            ExcessScale::Round(mode) => dec.round_to(scale, mode),
        }
    } else {
        dec
    };
    // 範囲は正規化後の値で検証する（§4.2 規則 2）。境界は validate_spec で有限と確認済み。
    if let Some(lo) = spec.min.and_then(Dec::from_f64) {
        if normalized < lo {
            return Err(DataError::Invalid(format!(
                "'{name}' は {} 以上である必要があります",
                lo.canonical()
            )));
        }
    }
    if let Some(hi) = spec.max.and_then(Dec::from_f64) {
        if normalized > hi {
            return Err(DataError::Invalid(format!(
                "'{name}' は {} 以下である必要があります",
                hi.canonical()
            )));
        }
    }
    to_json_number(&normalized).ok_or_else(|| {
        DataError::Invalid(format!(
            "'{name}' の有効桁数が多すぎます（本体と索引で同じ値を保てません）"
        ))
    })
}

/// 正規化値を JSON 数値へ戻す。整数は i64、それ以外は f64。f64 の最短表記で同じ値へ戻らない
/// （有効桁が多すぎる）なら `None`（§4.2 規則 3: 本体と索引の値をずらさない）。
fn to_json_number(d: &Dec) -> Option<Value> {
    let canonical = d.canonical();
    if d.frac.is_empty() {
        if let Ok(i) = canonical.parse::<i64>() {
            return Some(Value::Number(i.into()));
        }
    }
    let f: f64 = canonical.parse().ok()?;
    if Dec::from_f64(f)? != *d {
        return None;
    }
    Number::from_f64(f).map(Value::Number)
}

#[cfg(test)]
#[path = "numeric_tests.rs"]
mod tests;
