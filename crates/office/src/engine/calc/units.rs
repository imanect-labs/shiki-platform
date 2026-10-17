//! 正規化の補助（単位換算・語彙の写像・検証）（#725）。

use serde_json::{Map, Value};

use super::ops::{AlignSpec, ChartKind, CompareOp, HAlign, Paper, VAlign};
use crate::engine::a1::{self, Area};

/// `$A$1:$E$5` の形（名前・入力規則の参照元は絶対参照にする）。
pub(super) fn absolute(a: &Area) -> String {
    let c = |col: u32, row: u32| format!("${}${}", a1::col_name(col), row + 1);
    if a.sc == a.ec && a.sr == a.er {
        c(a.sc, a.sr)
    } else {
        format!("{}:{}", c(a.sc, a.sr), c(a.ec, a.er))
    }
}

pub(super) fn align_json(a: &AlignSpec) -> Value {
    let mut m = Map::new();
    if let Some(h) = a.horizontal {
        let v = match h {
            HAlign::General => "general",
            HAlign::Left => "left",
            HAlign::Center => "center",
            HAlign::Right => "right",
            HAlign::Justify => "justify",
        };
        m.insert("horizontal".into(), v.into());
    }
    if let Some(v) = a.vertical {
        let v = match v {
            VAlign::Top => "top",
            VAlign::Center => "center",
            VAlign::Bottom => "bottom",
        };
        m.insert("vertical".into(), v.into());
    }
    if let Some(w) = a.wrap {
        m.insert("wrap".into(), w.into());
    }
    if let Some(s) = a.shrink {
        m.insert("shrink".into(), s.into());
    }
    if let Some(i) = a.indent {
        m.insert("indent".into(), i.into());
    }
    if let Some(r) = a.rotate {
        m.insert("rotate".into(), (r.rem_euclid(360) * 100).into());
    }
    Value::Object(m)
}

pub(super) fn condition_operator(op: CompareOp) -> &'static str {
    match op {
        CompareOp::Eq => "EQUAL",
        CompareOp::Ne => "NOT_EQUAL",
        CompareOp::Lt => "LESS",
        CompareOp::Le => "LESS_EQUAL",
        CompareOp::Gt => "GREATER",
        CompareOp::Ge => "GREATER_EQUAL",
        CompareOp::Between => "BETWEEN",
        CompareOp::NotBetween => "NOT_BETWEEN",
    }
}

pub(super) fn chart_kind(k: ChartKind) -> &'static str {
    match k {
        ChartKind::Column => "column",
        ChartKind::Bar => "bar",
        ChartKind::Line => "line",
        ChartKind::Area => "area",
        ChartKind::Pie => "pie",
        ChartKind::Doughnut => "doughnut",
        ChartKind::Scatter => "scatter",
    }
}

/// 用紙の寸法（1/100 mm・縦置き）。B 判は JIS。
pub(super) fn paper_size(p: Paper) -> [u32; 2] {
    match p {
        Paper::A3 => [29_700, 42_000],
        Paper::A4 => [21_000, 29_700],
        Paper::A5 => [14_800, 21_000],
        Paper::B4 => [25_700, 36_400],
        Paper::B5 => [18_200, 25_700],
        Paper::Letter => [21_590, 27_940],
    }
}

/// `#RRGGBB`（`#` 省略・小文字も可）→ `#RRGGBB`。
pub fn normalize_color(c: &str) -> Option<String> {
    let h = c.trim().trim_start_matches('#');
    (h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| format!("#{}", h.to_ascii_uppercase()))
}

/// Excel の列幅（文字数）→ 1/100 mm（Calibri 11pt・最大桁幅 7px・96 dpi の換算）。
pub fn chars_to_mm100(chars: f64) -> u32 {
    let px = (chars * 7.0 + 5.0).round();
    float_to_u32(px / 96.0 * 2540.0)
}

/// 1/100 mm → Excel の列幅（文字数・小数 1 桁）。
pub fn mm100_to_chars(mm100: u32) -> f64 {
    let px = f64::from(mm100) / 2540.0 * 96.0;
    (((px - 5.0) / 7.0).max(0.0) * 10.0).round() / 10.0
}

pub fn pt_to_mm100(pt: f64) -> u32 {
    float_to_u32(pt * 2540.0 / 72.0)
}

pub fn mm100_to_pt(mm100: u32) -> f64 {
    (f64::from(mm100) * 72.0 / 2540.0 * 10.0).round() / 10.0
}

pub(super) fn cm_to_mm100(cm: f64) -> u32 {
    float_to_u32(cm * 1000.0)
}

fn float_to_u32(v: f64) -> u32 {
    // 値域は呼び出し側で検証済み（列幅・行高・寸法）。
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = v.round().clamp(0.0, f64::from(u32::MAX)) as u32;
    n
}

pub(super) fn trim_float(v: f64) -> String {
    let s = format!("{v}");
    s.strip_suffix(".0").map_or(s.clone(), str::to_string)
}

/// `YYYY-MM-DD`（`/` 区切りも可・時刻 `HH:MM[:SS]` つき可）→ (シリアル値, 時刻つきか)。
pub fn date_serial(s: &str) -> Option<(f64, bool)> {
    let s = s.trim().replace('/', "-");
    let (date, time) = match s.split_once([' ', 'T']) {
        Some((d, t)) => (d, Some(t)),
        None => (s.as_str(), None),
    };
    let d = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let base = chrono::NaiveDate::from_ymd_opt(1899, 12, 30)?;
    #[allow(clippy::cast_precision_loss)] // 日数は 10^6 未満。
    let mut serial = (d - base).num_days() as f64;
    if let Some(t) = time {
        let t = chrono::NaiveTime::parse_from_str(t, "%H:%M:%S")
            .or_else(|_| chrono::NaiveTime::parse_from_str(t, "%H:%M"))
            .ok()?;
        serial += f64::from(chrono::Timelike::num_seconds_from_midnight(&t)) / 86_400.0;
    }
    Some((serial, time.is_some()))
}

/// 定義できる名前か（セル参照と紛れる名前・空白や記号を含む名前は不可）。
pub(super) fn is_valid_name(name: &str) -> bool {
    let first_ok = name
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_');
    let rest_ok = name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '.');
    let looks_like_cell = a1::parse_area(name).is_ok();
    first_ok && rest_ok && !looks_like_cell && name.chars().count() <= 255
}
