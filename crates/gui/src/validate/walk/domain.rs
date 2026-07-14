//! ドメインカード（PR6）の検証（[`Walk`](super::Walk) の一部・ファイル分割）。
//!
//! 表示専用のため検証対象は文字列長・個数・数値有限性・列一致・URL スキーム（https のみ）。

use super::Walk;
use crate::domain::{
    ComparisonProps, ItineraryProps, SourceCardProps, TimelineProps, WeatherProps,
};
use crate::validate::{limits, GuiValidationError};

impl Walk<'_> {
    /// RAG 引用元カード。出典 URL は https のみ・スコアは有限数。
    pub(super) fn source_card(&mut self, p: &SourceCardProps, path: &str) {
        self.opt_label(p.title.as_deref(), &format!("{path}.title"));
        self.count(p.sources.len(), &format!("{path}.sources"));
        for (i, s) in p.sources.iter().enumerate() {
            let sp = format!("{path}.sources[{i}]");
            self.label(&s.title, &format!("{sp}.title"));
            self.opt_label(s.label.as_deref(), &format!("{sp}.label"));
            if let Some(snippet) = &s.snippet {
                self.text(snippet, limits::MAX_TEXT_CHARS, &format!("{sp}.snippet"));
            }
            if let Some(url) = &s.url {
                self.https_url(url, &format!("{sp}.url"));
            }
            if s.score.is_some_and(|v| !v.is_finite()) {
                self.errors.push(
                    GuiValidationError::new("gui.invalid_number", "score は有限数のみ")
                        .at(format!("{sp}.score")),
                );
            }
        }
    }

    /// 旅程カード。日／予定の個数と文字列長を検証。
    pub(super) fn itinerary(&mut self, p: &ItineraryProps, path: &str) {
        self.opt_label(p.title.as_deref(), &format!("{path}.title"));
        self.count(p.days.len(), &format!("{path}.days"));
        for (i, day) in p.days.iter().enumerate() {
            let dp = format!("{path}.days[{i}]");
            self.opt_label(day.label.as_deref(), &format!("{dp}.label"));
            self.opt_label(day.date.as_deref(), &format!("{dp}.date"));
            self.count(day.items.len(), &format!("{dp}.items"));
            for (j, it) in day.items.iter().enumerate() {
                let ip = format!("{dp}.items[{j}]");
                self.opt_label(it.time.as_deref(), &format!("{ip}.time"));
                self.label(&it.title, &format!("{ip}.title"));
                self.opt_label(it.location.as_deref(), &format!("{ip}.location"));
                if let Some(d) = &it.description {
                    self.text(d, limits::MAX_TEXT_CHARS, &format!("{ip}.description"));
                }
            }
        }
    }

    /// 天気カード。降水確率 0〜100・気温は有限数。
    pub(super) fn weather(&mut self, p: &WeatherProps, path: &str) {
        self.label(&p.location, &format!("{path}.location"));
        self.opt_label(p.title.as_deref(), &format!("{path}.title"));
        self.count(p.days.len(), &format!("{path}.days"));
        for (i, d) in p.days.iter().enumerate() {
            let dp = format!("{path}.days[{i}]");
            self.label(&d.label, &format!("{dp}.label"));
            for (v, name) in [(d.high, "high"), (d.low, "low")] {
                if v.is_some_and(|x| !x.is_finite()) {
                    self.errors.push(
                        GuiValidationError::new("gui.invalid_number", "気温は有限数のみ")
                            .at(format!("{dp}.{name}")),
                    );
                }
            }
            if d.precipitation
                .is_some_and(|x| !(x.is_finite() && (0.0..=100.0).contains(&x)))
            {
                self.errors.push(
                    GuiValidationError::new("gui.invalid_range", "precipitation は 0〜100")
                        .at(format!("{dp}.precipitation")),
                );
            }
        }
    }

    /// 比較カード。列数上限・各行の values は列数と一致・highlight は列範囲内。
    pub(super) fn comparison(&mut self, p: &ComparisonProps, path: &str) {
        self.opt_label(p.title.as_deref(), &format!("{path}.title"));
        if p.columns.is_empty() {
            self.errors.push(
                GuiValidationError::new("gui.empty_comparison", "columns は 1 列以上必要です")
                    .at(path),
            );
        }
        if p.columns.len() > limits::MAX_COMPARISON_COLS {
            self.errors.push(
                GuiValidationError::new(
                    "gui.too_many_columns",
                    format!("列が多すぎます（最大 {}）", limits::MAX_COMPARISON_COLS),
                )
                .at(path),
            );
        }
        for (i, c) in p.columns.iter().enumerate() {
            self.label(c, &format!("{path}.columns[{i}]"));
        }
        self.count(p.rows.len(), &format!("{path}.rows"));
        for (i, row) in p.rows.iter().enumerate() {
            let rp = format!("{path}.rows[{i}]");
            self.label(&row.label, &format!("{rp}.label"));
            if row.values.len() != p.columns.len() {
                self.errors.push(
                    GuiValidationError::new(
                        "gui.comparison_row_mismatch",
                        format!(
                            "values の数（{}）が列数（{}）と一致しません",
                            row.values.len(),
                            p.columns.len()
                        ),
                    )
                    .at(&rp),
                );
            }
            for (j, v) in row.values.iter().enumerate() {
                self.text(v, limits::MAX_TEXT_CHARS, &format!("{rp}.values[{j}]"));
            }
        }
        if p.highlight.is_some_and(|h| (h as usize) >= p.columns.len()) {
            self.errors.push(
                GuiValidationError::new(
                    "gui.invalid_range",
                    "highlight は列の範囲内で指定してください",
                )
                .at(format!("{path}.highlight")),
            );
        }
    }

    /// タイムライン。イベント数と文字列長を検証。
    pub(super) fn timeline(&mut self, p: &TimelineProps, path: &str) {
        self.opt_label(p.title.as_deref(), &format!("{path}.title"));
        self.count(p.events.len(), &format!("{path}.events"));
        for (i, e) in p.events.iter().enumerate() {
            let ep = format!("{path}.events[{i}]");
            self.opt_label(e.time.as_deref(), &format!("{ep}.time"));
            self.label(&e.title, &format!("{ep}.title"));
            if let Some(d) = &e.description {
                self.text(d, limits::MAX_TEXT_CHARS, &format!("{ep}.description"));
            }
        }
    }

    /// https のみの URL 検証（javascript:/data:/相対 URL を構造的に拒否・補正なし）。
    fn https_url(&mut self, url: &str, path: &str) {
        if url.chars().count() > limits::MAX_URL_CHARS {
            self.errors
                .push(GuiValidationError::new("gui.string_too_long", "URL が長すぎます").at(path));
        }
        if !url.starts_with("https://") {
            self.errors.push(
                GuiValidationError::new(
                    "gui.forbidden_url_scheme",
                    "URL は https:// のみ使用できます",
                )
                .at(path),
            );
        }
    }
}
