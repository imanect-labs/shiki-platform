//! グラフの見た目の検証と正規化（Excel の add_chart と Word の insert_chart で共有・#731）。
//!
//! 種類・大きさ・題・凡例・配色・ラベル・軸の題は、エンジンの `shapeChart`（engine/chart.js）が
//! 両方の文書で同じ形で受け取る。ここではその形へ揃える（データの持ち方は呼び出し側）。

use serde_json::{json, Map, Value};

use super::calc::normalize::normalize_color;
use super::calc::ops::{ChartKind, LegendPosition};
use super::calc::units::{chart_kind, cm_to_mm100};

/// グラフの見た目（op の項目をそのまま借りる）。
#[allow(clippy::struct_excessive_bools)] // op の真偽の項目（凡例・ラベル・積み上げ・割合）をそのまま写す。
pub struct ChartLook<'a> {
    pub kind: ChartKind,
    pub title: Option<&'a str>,
    pub width_cm: Option<f64>,
    pub height_cm: Option<f64>,
    pub legend: bool,
    pub legend_position: Option<LegendPosition>,
    pub colors: Option<&'a [String]>,
    pub data_labels: bool,
    pub y_number_format: Option<&'a str>,
    pub stacked: bool,
    pub percent: bool,
    pub x_title: Option<&'a str>,
    pub y_title: Option<&'a str>,
}

impl ChartLook<'_> {
    /// エンジン向けの項目（kind・size・title・legend_position・colors・…）。誤りは利用者向けの文。
    pub fn normalize(&self) -> Result<Map<String, Value>, String> {
        let w = self.width_cm.unwrap_or(15.0);
        let h = self.height_cm.unwrap_or(9.0);
        if !(3.0..=60.0).contains(&w) || !(3.0..=60.0).contains(&h) {
            return Err("グラフの大きさは 3〜60 cm で指定してください".into());
        }
        let mut palette = Vec::new();
        for c in self.colors.into_iter().flatten() {
            palette.push(
                normalize_color(c)
                    .ok_or_else(|| format!("色「{c}」は #RRGGBB で書いてください"))?,
            );
        }
        let legend_position = match (self.legend, self.legend_position) {
            (false, _) | (_, Some(LegendPosition::None)) => "none",
            (_, Some(LegendPosition::Right)) => "right",
            (_, Some(LegendPosition::Top)) => "top",
            (_, Some(LegendPosition::Left)) => "left",
            _ => "bottom",
        };
        let Value::Object(map) = json!({
            "kind": chart_kind(self.kind), "size": [cm_to_mm100(w), cm_to_mm100(h)],
            "title": self.title, "legend_position": legend_position, "colors": palette,
            "data_labels": self.data_labels, "y_number_format": self.y_number_format,
            "stacked": self.stacked, "percent": self.percent,
            "x_title": self.x_title, "y_title": self.y_title,
        }) else {
            unreachable!("json! のオブジェクト")
        };
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(kind: ChartKind) -> ChartLook<'static> {
        ChartLook {
            kind,
            title: Some("売上"),
            width_cm: None,
            height_cm: None,
            legend: true,
            legend_position: None,
            colors: None,
            data_labels: false,
            y_number_format: None,
            stacked: false,
            percent: false,
            x_title: None,
            y_title: None,
        }
    }

    #[test]
    fn defaults_and_legend() {
        let m = look(ChartKind::Column).normalize().unwrap();
        assert_eq!(m["kind"], "column");
        assert_eq!(m["size"], json!([15000, 9000]));
        assert_eq!(m["legend_position"], "bottom");
        let mut l = look(ChartKind::Pie);
        l.legend = false;
        assert_eq!(l.normalize().unwrap()["legend_position"], "none");
        l.legend = true;
        l.legend_position = Some(LegendPosition::Right);
        assert_eq!(l.normalize().unwrap()["legend_position"], "right");
    }

    #[test]
    fn rejects_bad_size_and_colors() {
        let mut l = look(ChartKind::Line);
        l.width_cm = Some(2.0);
        assert!(l.normalize().unwrap_err().contains("3〜60 cm"));
        let colors = vec!["#4472c4".to_string(), "blue!".to_string()];
        let mut l = look(ChartKind::Line);
        l.colors = Some(&colors);
        assert!(l.normalize().unwrap_err().contains("blue!"));
        let colors = vec!["#4472c4".to_string()];
        l.colors = Some(&colors);
        assert_eq!(l.normalize().unwrap()["colors"], json!(["#4472C4"]));
    }
}
