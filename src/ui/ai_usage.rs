use std::cell::RefCell;
use std::f64::consts::{FRAC_PI_2, TAU};
use std::rc::Rc;

use crate::{AiProviderUsage, AiQuotaEstimate, AiUsageState, SourceHealth, TokenUsage};
use gtk::prelude::*;
use gtk4 as gtk;
use gtk4::glib::DateTime;

const TOKEN_KINDS: [(&str, &str); 4] = [
    ("Fresh input", "cockpit_text"),
    ("Cache read", "primary"),
    ("Cache write", "cockpit_muted"),
    ("Output", "tertiary"),
];

pub(super) struct AiUsageView {
    pub root: gtk::Box,
    total: gtk::Label,
    date: gtk::Label,
    shares: [gtk::Label; 2],
    ring: UsageChart,
    providers: [ProviderCard; 2],
    health: gtk::Label,
    previous: RefCell<Option<(AiUsageState, Option<SourceHealth>)>>,
}

impl AiUsageView {
    pub fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.add_css_class("ai-usage-page");
        let hero = gtk::Box::new(gtk::Orientation::Horizontal, 24);
        hero.add_css_class("ai-usage-hero");
        let dots = [swatch("codex"), swatch("claude")];
        let ring = UsageChart::new(
            true,
            vec![("primary", dots[0].clone()), ("tertiary", dots[1].clone())],
        );
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&ring.area));
        let center = gtk::Box::new(gtk::Orientation::Vertical, 0);
        center.set_halign(gtk::Align::Center);
        center.set_valign(gtk::Align::Center);
        let total = text("—", "ai-total");
        total.set_xalign(0.5);
        center.append(&total);
        let unit = text("tokens", "ai-caption");
        unit.set_xalign(0.5);
        center.append(&unit);
        overlay.add_overlay(&center);
        hero.append(&overlay);
        let summary = gtk::Box::new(gtk::Orientation::Vertical, 8);
        summary.set_valign(gtk::Align::Center);
        summary.set_hexpand(true);
        summary.append(&text("TODAY'S USAGE", "ai-eyebrow"));
        let date = text("", "ai-caption");
        summary.append(&date);
        let shares = [text("", "ai-share"), text("", "ai-share")];
        for (index, name) in ["Codex", "Claude Code"].into_iter().enumerate() {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.append(&dots[index]);
            let name = text(name, "ai-provider-name");
            name.set_hexpand(true);
            row.append(&name);
            row.append(&shares[index]);
            summary.append(&row);
        }
        hero.append(&summary);
        root.append(&hero);
        let health = text("", "ai-health");
        health.set_wrap(true);
        health.set_visible(false);
        root.append(&health);
        let providers = [
            ProviderCard::new("Codex", "codex"),
            ProviderCard::new("Claude Code", "claude"),
        ];
        for provider in &providers {
            root.append(&provider.root);
        }
        let note = text(
            "Includes cached tokens · local sessions · updates every 15 s",
            "ai-caption",
        );
        note.set_wrap(true);
        root.append(&note);
        Self {
            root,
            total,
            date,
            shares,
            ring,
            providers,
            health,
            previous: RefCell::new(None),
        }
    }

    pub fn update(&self, usage: &AiUsageState, health: Option<&SourceHealth>) {
        let next = (usage.clone(), health.cloned());
        if self.previous.borrow().as_ref() == Some(&next) {
            return;
        }
        self.previous.replace(Some(next));
        let total = usage
            .codex
            .tokens
            .total_tokens
            .saturating_add(usage.claude.tokens.total_tokens);
        let available = usage.codex.available || usage.claude.available;
        self.total.set_label(&if available {
            compact_tokens(total)
        } else {
            "—".to_string()
        });
        self.total
            .set_tooltip_text(Some(&format!("{} tokens today", grouped(total))));
        self.date.set_label(&usage.day);
        self.ring.update(
            vec![
                ("primary", usage.codex.tokens.total_tokens),
                ("tertiary", usage.claude.tokens.total_tokens),
            ],
            &format!(
                "Today's tokens: Codex {}, Claude Code {}",
                grouped(usage.codex.tokens.total_tokens),
                grouped(usage.claude.tokens.total_tokens)
            ),
        );
        for (index, provider) in [&usage.codex, &usage.claude].into_iter().enumerate() {
            self.shares[index].set_label(&if !provider.available {
                "—".to_string()
            } else if total == 0 {
                "0%".to_string()
            } else {
                format!(
                    "{:.0}%",
                    provider.tokens.total_tokens as f64 * 100.0 / total as f64
                )
            });
            self.providers[index].update(provider, health);
        }
        let message = match health {
            Some(SourceHealth::Disconnected { message }) => {
                Some(format!("Usage unavailable · {message}"))
            }
            Some(SourceHealth::Stale { .. }) => {
                Some("Showing the last usage snapshot · waiting for an update".to_string())
            }
            _ => None,
        };
        self.health.set_visible(message.is_some());
        self.health
            .set_label(message.as_deref().unwrap_or_default());
    }
}

struct ProviderCard {
    root: gtk::Box,
    amount: gtk::Label,
    activity: gtk::Label,
    chart: UsageChart,
    metrics: gtk::Grid,
    values: [gtk::Label; 4],
    empty: gtk::Label,
    reasoning: gtk::Label,
    quotas: gtk::Box,
}

impl ProviderCard {
    fn new(name: &str, class: &str) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.add_css_class("ai-provider-card");
        root.add_css_class(class);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.append(&swatch(class));
        let title = text(name, "ai-provider-title");
        title.set_hexpand(true);
        header.append(&title);
        let amount = text("—", "ai-provider-total");
        header.append(&amount);
        root.append(&header);
        let dots = ["fresh", "cached", "written", "output"].map(swatch);
        let chart = UsageChart::new(
            false,
            TOKEN_KINDS
                .iter()
                .zip(&dots)
                .map(|((_, role), dot)| (*role, dot.clone()))
                .collect(),
        );
        root.append(&chart.area);
        let metrics = gtk::Grid::new();
        metrics.set_column_homogeneous(true);
        metrics.set_column_spacing(16);
        metrics.set_row_spacing(8);
        let values = std::array::from_fn(|_| text("0", "ai-metric-value"));
        for (index, (name, _)) in TOKEN_KINDS.iter().enumerate() {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            row.append(&dots[index]);
            let title = text(name, "ai-caption");
            title.set_hexpand(true);
            row.append(&title);
            row.append(&values[index]);
            metrics.attach(&row, (index % 2) as i32, (index / 2) as i32, 1, 1);
        }
        root.append(&metrics);
        let empty = text("No tokens recorded today", "ai-empty");
        root.append(&empty);
        let reasoning = text("", "ai-caption");
        reasoning.set_wrap(true);
        root.append(&reasoning);
        let quotas = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.append(&quotas);
        let activity = text("", "ai-activity");
        root.append(&activity);
        Self {
            root,
            amount,
            activity,
            chart,
            metrics,
            values,
            empty,
            reasoning,
            quotas,
        }
    }

    fn update(&self, usage: &AiProviderUsage, health: Option<&SourceHealth>) {
        let tokens = &usage.tokens;
        let nonzero = usage.available && tokens.total_tokens > 0;
        self.amount.set_label(&if usage.available {
            compact_tokens(tokens.total_tokens)
        } else {
            "—".to_string()
        });
        self.amount
            .set_tooltip_text(Some(&format!("{} tokens", grouped(tokens.total_tokens))));
        self.metrics.set_visible(nonzero);
        self.empty.set_visible(!nonzero);
        self.empty.set_label(if usage.available {
            "No tokens recorded today"
        } else {
            "No local session logs found"
        });
        let parts = token_parts(tokens);
        let chart_label = TOKEN_KINDS
            .iter()
            .zip(parts)
            .map(|((name, _), value)| format!("{name}: {} tokens", grouped(value)))
            .collect::<Vec<_>>()
            .join(" · ");
        self.chart.update(
            TOKEN_KINDS
                .iter()
                .zip(parts)
                .map(|((_, role), value)| (*role, value))
                .collect(),
            &chart_label,
        );
        for (index, value) in parts.into_iter().enumerate() {
            self.values[index].set_label(&compact_tokens(value));
            self.values[index].set_tooltip_text(Some(&format!("{} tokens", grouped(value))));
        }
        self.reasoning
            .set_visible(tokens.reasoning_output_tokens > 0);
        self.reasoning.set_label(&format!(
            "{} reasoning tokens included in output",
            compact_tokens(tokens.reasoning_output_tokens)
        ));
        self.reasoning.set_tooltip_text(Some(&format!(
            "{} reasoning tokens",
            grouped(tokens.reasoning_output_tokens)
        )));
        self.activity.set_visible(usage.last_event_at.is_some());
        self.activity.set_label(
            &usage
                .last_event_at
                .map(|epoch| format!("Last activity {}", local_time(epoch)))
                .unwrap_or_default(),
        );
        while let Some(child) = self.quotas.first_child() {
            self.quotas.remove(&child);
        }
        self.quotas.set_visible(usage.available);
        if usage.quotas.is_empty() {
            let note = text(
                "Time left unavailable · no quota data in local logs",
                "ai-caption",
            );
            note.set_wrap(true);
            self.quotas.append(&note);
        }
        for quota in &usage.quotas {
            let group = gtk::Box::new(gtk::Orientation::Vertical, 6);
            group.add_css_class("ai-quota");
            let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let title = text(
                &format!("{} quota", quota_window(quota.window_minutes)),
                "ai-caption",
            );
            title.set_hexpand(true);
            header.append(&title);
            header.append(&text(
                &format!("{}% used", quota.used_percent),
                "ai-quota-value",
            ));
            group.append(&header);
            let meter = gtk::ProgressBar::new();
            meter.add_css_class("ai-quota-meter");
            if quota.used_percent >= 95 {
                meter.add_css_class("critical");
            } else if quota.used_percent >= 80 {
                meter.add_css_class("warning");
            }
            meter.set_fraction(f64::from(quota.used_percent.min(100)) / 100.0);
            meter.update_property(&[gtk::accessible::Property::Label(&format!(
                "{} quota: {} percent used",
                quota_window(quota.window_minutes),
                quota.used_percent
            ))]);
            group.append(&meter);
            let reset = quota
                .resets_at
                .map(|epoch| format!("Resets {}", local_time(epoch)))
                .unwrap_or_else(|| "Reset time unavailable".to_string());
            let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let reset = text(&reset, "ai-caption");
            reset.set_hexpand(true);
            footer.append(&reset);
            footer.append(&text(
                &format!("{}% left", 100_u8.saturating_sub(quota.used_percent)),
                "ai-caption",
            ));
            group.append(&footer);
            group.append(&estimate_card(usable_estimate(&quota.estimate, health)));
            if let Some(epoch) = usage.quota_observed_at {
                let report = format!("Last reported {}", local_time(epoch));
                group.append(&text(&report, "ai-activity"));
            }
            self.quotas.append(&group);
        }
    }
}

struct EstimatePresentation {
    title: String,
    detail: String,
    outlook: String,
    class: &'static str,
}

fn usable_estimate<'a>(
    estimate: &'a AiQuotaEstimate,
    health: Option<&SourceHealth>,
) -> &'a AiQuotaEstimate {
    if matches!(
        health,
        Some(SourceHealth::Stale { .. } | SourceHealth::Disconnected { .. })
    ) {
        &AiQuotaEstimate::Stale
    } else {
        estimate
    }
}

fn estimate_presentation(estimate: &AiQuotaEstimate) -> EstimatePresentation {
    let (title, detail, outlook, class) = match estimate {
        AiQuotaEstimate::Available {
            remaining_seconds,
            exhausts_at,
            sample_seconds,
            used_percent_delta,
            lasts_until_reset,
        } => {
            let pace = f64::from(*used_percent_delta) * 3600.0 / (*sample_seconds).max(1) as f64;
            let outlook = match lasts_until_reset {
                Some(true) => "Should last until reset at this pace".to_string(),
                Some(false) => format!("May run out {} · before reset", local_time(*exhausts_at)),
                None => format!("May run out {}", local_time(*exhausts_at)),
            };
            (
                format!("≈ {} left", estimate_duration(*remaining_seconds)),
                format!("{pace:.1}% of quota/h · last {} min", sample_seconds / 60),
                outlook,
                if *lasts_until_reset == Some(true) {
                    "on-track"
                } else {
                    "at-risk"
                },
            )
        }
        AiQuotaEstimate::Collecting => (
            "Learning your pace".to_string(),
            "Needs 15+ minutes of measurable quota usage".to_string(),
            String::new(),
            "pending",
        ),
        AiQuotaEstimate::Stale => (
            "Estimate paused".to_string(),
            "Waiting for a recent quota report".to_string(),
            String::new(),
            "pending",
        ),
        AiQuotaEstimate::ResetDue => (
            "Reset due".to_string(),
            "Waiting for updated quota data".to_string(),
            String::new(),
            "pending",
        ),
        AiQuotaEstimate::Exhausted => (
            "Quota used up".to_string(),
            "Latest report shows 100% used".to_string(),
            String::new(),
            "at-risk",
        ),
        AiQuotaEstimate::NoRecentBurn { sample_seconds } => (
            "No recent quota increase".to_string(),
            format!(
                "Not enough change over {} min to estimate time left",
                sample_seconds / 60
            ),
            String::new(),
            "pending",
        ),
    };
    EstimatePresentation {
        title,
        detail,
        outlook,
        class,
    }
}

fn estimate_card(estimate: &AiQuotaEstimate) -> gtk::Box {
    let presentation = estimate_presentation(estimate);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
    root.add_css_class("ai-estimate");
    root.add_css_class(presentation.class);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let icon = gtk::Image::from_icon_name("alarm-symbolic");
    icon.set_pixel_size(20);
    row.append(&icon);
    let title = text(&presentation.title, "ai-estimate-title");
    title.set_wrap(true);
    title.set_hexpand(true);
    row.append(&title);
    root.append(&row);
    let detail = text(&presentation.detail, "ai-caption");
    detail.set_wrap(true);
    root.append(&detail);
    if !presentation.outlook.is_empty() {
        let outlook = text(&presentation.outlook, "ai-estimate-outlook");
        outlook.set_wrap(true);
        root.append(&outlook);
    }
    root
}

fn estimate_duration(seconds: u64) -> String {
    if seconds < 60 {
        return "<1m".to_string();
    }
    // The input quota is rounded to whole percentages; avoid false precision.
    let minutes = if seconds >= 3600 {
        seconds.div_ceil(300) * 5
    } else {
        seconds.div_ceil(60)
    };
    if minutes >= 1440 {
        format!("{}d {}h", minutes / 1440, minutes % 1440 / 60)
    } else if minutes >= 60 {
        format!("{}h {}m", minutes / 60, minutes % 60)
    } else {
        format!("{minutes}m")
    }
}

// Cache reads/writes are subsets of input; reasoning is a subset of output.
fn token_parts(tokens: &TokenUsage) -> [u64; 4] {
    let cached = tokens.cached_input_tokens.min(tokens.input_tokens);
    let written = tokens
        .cache_write_input_tokens
        .min(tokens.input_tokens - cached);
    [
        tokens.input_tokens - cached - written,
        cached,
        written,
        tokens.output_tokens,
    ]
}

fn text(value: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(value));
    label.set_xalign(0.0);
    label.add_css_class(class);
    label
}

fn swatch(class: &str) -> gtk::Box {
    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("ai-swatch");
    dot.add_css_class(class);
    dot.set_valign(gtk::Align::Center);
    dot
}

type ChartParts = Rc<RefCell<Vec<(&'static str, u64)>>>;

struct UsageChart {
    area: gtk::DrawingArea,
    parts: ChartParts,
}

impl UsageChart {
    fn new(ring: bool, palette: Vec<(&'static str, gtk::Box)>) -> Self {
        let area = gtk::DrawingArea::new();
        area.add_css_class("ai-usage-chart");
        area.set_content_height(if ring { 140 } else { 12 });
        if ring {
            area.set_content_width(140);
        } else {
            area.set_hexpand(true);
        }
        let parts = Rc::new(RefCell::new(Vec::<(&'static str, u64)>::new()));
        let draw_parts = parts.clone();
        area.set_draw_func(move |area, context, width, height| {
            let colors = draw_parts
                .borrow()
                .iter()
                .map(|(role, count)| {
                    let color = palette
                        .iter()
                        .find(|(name, _)| name == role)
                        .map(|(_, widget)| widget.color())
                        .unwrap_or_else(|| area.color());
                    (*count, color)
                })
                .collect::<Vec<_>>();
            draw_chart(area, context, width, height, &colors, ring);
        });
        Self { area, parts }
    }

    fn update(&self, parts: Vec<(&'static str, u64)>, description: &str) {
        self.parts.replace(parts);
        self.area.set_tooltip_text(Some(description));
        self.area
            .update_property(&[gtk::accessible::Property::Label(description)]);
        self.area.queue_draw();
    }
}

fn draw_chart(
    area: &gtk::DrawingArea,
    context: &gtk::cairo::Context,
    width: i32,
    height: i32,
    parts: &[(u64, gtk::gdk::RGBA)],
    ring: bool,
) {
    let (width, height) = (f64::from(width), f64::from(height));
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let radius = (width.min(height) - 16.0) / 2.0;
    let color = area.color();
    context.set_source_rgba(
        f64::from(color.red()),
        f64::from(color.green()),
        f64::from(color.blue()),
        0.08,
    );
    if ring {
        context.set_line_width(10.0);
        context.arc(width / 2.0, height / 2.0, radius, 0.0, TAU);
        let _ = context.stroke();
    } else {
        // A clipped capsule keeps real segment proportions even for tiny output slices.
        let r = height / 2.0;
        context.arc(width - r, r, r, -FRAC_PI_2, FRAC_PI_2);
        context.arc(r, r, r, FRAC_PI_2, FRAC_PI_2 * 3.0);
        context.close_path();
        context.clip();
        let _ = context.paint();
    }
    let total: f64 = parts.iter().map(|(value, _)| *value as f64).sum();
    if total == 0.0 {
        return;
    }
    let mut position = 0.0;
    for (value, color) in parts {
        let fraction = *value as f64 / total;
        if fraction == 0.0 {
            continue;
        }
        context.set_source_rgba(
            f64::from(color.red()),
            f64::from(color.green()),
            f64::from(color.blue()),
            1.0,
        );
        if ring {
            context.arc(
                width / 2.0,
                height / 2.0,
                radius,
                position * TAU - FRAC_PI_2,
                (position + fraction) * TAU - FRAC_PI_2,
            );
            let _ = context.stroke();
        } else {
            context.rectangle(position * width, 0.0, fraction * width, height);
            let _ = context.fill();
        }
        position += fraction;
    }
}

fn quota_window(minutes: u64) -> String {
    if minutes >= 1440 && minutes.is_multiple_of(1440) {
        format!("{} day", minutes / 1440)
    } else if minutes >= 60 && minutes.is_multiple_of(60) {
        format!("{} hour", minutes / 60)
    } else {
        format!("{minutes} minute")
    }
}

pub(super) fn compact_tokens(tokens: u64) -> String {
    for (threshold, suffix) in [(1_000_000_000, "B"), (1_000_000, "M"), (1_000, "k")] {
        if tokens >= threshold {
            return format!("{:.1}{suffix}", tokens as f64 / threshold as f64);
        }
    }
    tokens.to_string()
}

pub(super) fn label(usage: &AiUsageState, health: Option<&SourceHealth>) -> String {
    if !usage.codex.available && !usage.claude.available {
        return "AI —".to_string();
    }
    let mut summary = format!(
        "AI {}",
        compact_tokens(
            usage
                .codex
                .tokens
                .total_tokens
                .saturating_add(usage.claude.tokens.total_tokens)
        )
    );
    if usage.codex.quotas.iter().any(|quota| {
        matches!(
            usable_estimate(&quota.estimate, health),
            AiQuotaEstimate::Exhausted
        )
    }) {
        return format!("{summary} · quota used");
    }
    let limiting = usage
        .codex
        .quotas
        .iter()
        .filter_map(|quota| match usable_estimate(&quota.estimate, health) {
            AiQuotaEstimate::Available {
                remaining_seconds,
                lasts_until_reset,
                ..
            } if *lasts_until_reset != Some(true) => Some(*remaining_seconds),
            _ => None,
        })
        .min();
    if let Some(seconds) = limiting {
        summary.push_str(&format!(" · ~{}", estimate_duration(seconds)));
    }
    summary
}

pub(super) fn detail(usage: &AiUsageState, health: Option<&SourceHealth>) -> String {
    let mut lines = vec![format!("AI tokens today · {}", usage.day)];
    for (name, provider) in [("Codex", &usage.codex), ("Claude Code", &usage.claude)] {
        lines.push(if provider.available {
            format!("{name} · {} tokens", grouped(provider.tokens.total_tokens))
        } else {
            format!("{name} · no local session logs")
        });
        for quota in &provider.quotas {
            lines.push(format!(
                "{} quota: {}",
                quota_window(quota.window_minutes),
                estimate_presentation(usable_estimate(&quota.estimate, health)).title
            ));
        }
    }
    lines.push("Click for visual breakdown · input includes cache".to_string());
    lines.join("\n")
}

fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut result = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            result.push(',');
        }
        result.push(digit);
    }
    result
}

fn local_time(epoch: i64) -> String {
    DateTime::from_unix_local(epoch)
        .and_then(|date| date.format("%a %H:%M"))
        .map(|text| text.to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forecast_is_conditional_and_rounded_without_claiming_infinite_idle_usage() {
        assert_eq!(estimate_duration(7381), "2h 5m");
        assert_eq!(estimate_duration(0), "<1m");
        let estimate = AiQuotaEstimate::Available {
            remaining_seconds: 7200,
            exhausts_at: 100000,
            sample_seconds: 3600,
            used_percent_delta: 20,
            lasts_until_reset: Some(false),
        };
        let presentation = estimate_presentation(&estimate);
        assert_eq!(presentation.title, "≈ 2h 0m left");
        assert!(presentation.outlook.contains("before reset"));
        let idle = estimate_presentation(&AiQuotaEstimate::NoRecentBurn {
            sample_seconds: 3600,
        });
        assert!(idle.detail.contains("Not enough change"));
    }

    #[test]
    fn bar_uses_the_first_limiting_quota_and_omits_estimates_that_reset_first() {
        let mut usage = AiUsageState::default();
        usage.codex.available = true;
        usage.codex.quotas = [7200, 3600]
            .into_iter()
            .map(|remaining_seconds| crate::AiQuota {
                used_percent: 50,
                window_minutes: remaining_seconds,
                resets_at: Some(100000),
                estimate: AiQuotaEstimate::Available {
                    remaining_seconds,
                    exhausts_at: 90000,
                    sample_seconds: 3600,
                    used_percent_delta: 25,
                    lasts_until_reset: Some(false),
                },
            })
            .collect();
        assert_eq!(label(&usage, None), "AI 0 · ~1h 0m");
        assert_eq!(
            label(&usage, Some(&SourceHealth::Stale { since_epoch: 1 })),
            "AI 0"
        );
        for quota in &mut usage.codex.quotas {
            if let AiQuotaEstimate::Available {
                lasts_until_reset, ..
            } = &mut quota.estimate
            {
                *lasts_until_reset = Some(true);
            }
        }
        assert_eq!(label(&usage, None), "AI 0");
    }

    #[test]
    fn chart_segments_partition_input_and_output_without_counting_subsets_twice() {
        let usage = TokenUsage {
            input_tokens: 1000,
            cached_input_tokens: 600,
            cache_write_input_tokens: 100,
            output_tokens: 200,
            reasoning_output_tokens: 150,
            total_tokens: 1200,
        };
        assert_eq!(token_parts(&usage), [300, 600, 100, 200]);
        assert_eq!(token_parts(&usage).iter().sum::<u64>(), usage.total_tokens);
    }

    #[test]
    fn empty_and_inconsistent_cache_counts_do_not_create_invalid_segments() {
        assert_eq!(token_parts(&TokenUsage::default()), [0; 4]);
        let usage = TokenUsage {
            input_tokens: 10,
            cached_input_tokens: 20,
            cache_write_input_tokens: 30,
            ..TokenUsage::default()
        };
        assert_eq!(token_parts(&usage), [0, 10, 0, 0]);
    }

    #[test]
    fn combined_label_keeps_provider_totals_distinct_in_details() {
        let mut usage = AiUsageState {
            day: "2026-10-14".to_string(),
            ..AiUsageState::default()
        };
        assert_eq!(label(&usage, None), "AI —");
        usage.codex.available = true;
        usage.codex.tokens.total_tokens = 1_200_000;
        usage.claude.available = true;
        usage.claude.tokens.total_tokens = 300_000;
        assert_eq!(label(&usage, None), "AI 1.5M");
        let text = detail(&usage, None);
        assert!(text.contains("Codex · 1,200,000 tokens"));
        assert!(text.contains("Claude Code · 300,000 tokens"));
        assert!(text.contains("input includes cache"));
    }
}
