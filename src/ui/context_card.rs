use crate::{ActivityStatus, ContextCard, ContextTier, DesktopContext, PlaybackStatus};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextPresentation {
    pub icon_name: String,
    pub text: String,
    pub tier: ContextTier,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextAction {
    OpenControlCenter(DesktopContext),
    FocusWindow { output: String, window_id: String },
}

pub fn context_action(card: &ContextCard) -> Option<ContextAction> {
    match card {
        ContextCard::Battery { .. } => {
            Some(ContextAction::OpenControlCenter(DesktopContext::Power))
        }
        ContextCard::Calendar { .. } | ContextCard::Timer { .. } => {
            Some(ContextAction::OpenControlCenter(DesktopContext::Clock))
        }
        ContextCard::Media { .. } => Some(ContextAction::OpenControlCenter(DesktopContext::Audio)),
        ContextCard::Activity { .. } => None,
        ContextCard::Urgent {
            output,
            window_id: Some(window_id),
            ..
        } => Some(ContextAction::FocusWindow {
            output: output.clone(),
            window_id: window_id.clone(),
        }),
        ContextCard::Urgent { .. } => None,
    }
}

pub fn media_playback_icon(status: &PlaybackStatus) -> &'static str {
    match status {
        PlaybackStatus::Playing => "media-playback-pause-symbolic",
        PlaybackStatus::Paused | PlaybackStatus::Stopped => "media-playback-start-symbolic",
    }
}

pub fn context_tier(card: &ContextCard) -> ContextTier {
    match card {
        ContextCard::Battery { tier, .. }
        | ContextCard::Calendar { tier, .. }
        | ContextCard::Timer { tier, .. } => *tier,
        ContextCard::Activity { .. } => ContextTier::Work,
        ContextCard::Media { .. } => ContextTier::Ambient,
        ContextCard::Urgent { .. } => ContextTier::Critical,
    }
}

pub fn context_presentation(card: &ContextCard, now_epoch: i64) -> ContextPresentation {
    ContextPresentation {
        icon_name: context_icon_name(card).to_string(),
        text: context_text(card, now_epoch),
        tier: context_tier(card),
    }
}

fn context_icon_name(card: &ContextCard) -> &'static str {
    match card {
        ContextCard::Battery { .. } => "battery-caution-symbolic",
        ContextCard::Calendar { .. } => "appointment-soon-symbolic",
        ContextCard::Timer { .. } => "alarm-symbolic",
        ContextCard::Activity { .. } => "system-run-symbolic",
        ContextCard::Media { .. } => "media-playback-start-symbolic",
        ContextCard::Urgent { .. } => "dialog-warning-symbolic",
    }
}

pub fn context_text(card: &ContextCard, now_epoch: i64) -> String {
    match card {
        ContextCard::Battery {
            percent, charging, ..
        } => {
            if *charging {
                format!("Battery {percent}% charging")
            } else {
                format!("Battery {percent}%")
            }
        }
        ContextCard::Calendar {
            title,
            location,
            start_epoch,
            ..
        } => {
            let event = location
                .as_ref()
                .map(|location| format!("{title} — {location}"))
                .unwrap_or_else(|| title.clone());
            format!("{event} · {}", relative_time(*start_epoch, now_epoch))
        }
        ContextCard::Timer {
            label,
            remaining_seconds,
            completed,
            ..
        } => {
            if *completed {
                format!("{label} done")
            } else {
                format!("{label} · {}", clock_duration(*remaining_seconds))
            }
        }
        ContextCard::Activity { label, status, .. } => {
            let status = match status {
                ActivityStatus::Running => "running",
                ActivityStatus::Succeeded => "finished",
                ActivityStatus::Failed => "failed",
            };
            format!("{label} · {status}")
        }
        ContextCard::Media {
            title,
            artist,
            player,
            ..
        } => match (title, artist) {
            (Some(title), Some(artist)) => format!("{title} - {artist}"),
            (Some(title), None) => title.clone(),
            _ => player.clone(),
        },
        ContextCard::Urgent {
            window_title,
            workspace,
            output,
            ..
        } => window_title
            .clone()
            .or_else(|| workspace.clone())
            .unwrap_or_else(|| format!("Urgent on {output}")),
    }
}

pub fn warning_text(card: &ContextCard) -> String {
    match card {
        ContextCard::Urgent {
            window_title,
            workspace,
            output,
            ..
        } => window_title
            .clone()
            .or_else(|| workspace.clone())
            .unwrap_or_else(|| format!("Urgent on {output}")),
        other => context_text(other, 0),
    }
}

fn relative_time(start_epoch: i64, now_epoch: i64) -> String {
    let remaining = start_epoch.saturating_sub(now_epoch);
    if remaining <= 0 {
        return "now".to_string();
    }

    if remaining < 60 {
        return "in <1m".to_string();
    }

    let minutes = (remaining + 59) / 60;
    if minutes < 120 {
        return format!("in {minutes}m");
    }

    let hours = (minutes + 59) / 60;
    if hours < 48 {
        return format!("in {hours}h");
    }

    let days = (hours + 23) / 24;
    format!("in {days}d")
}

fn clock_duration(seconds: u64) -> String {
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::{ActivityStatus, ContextCard, ContextTier, DesktopContext, PlaybackStatus};

    use super::{ContextAction, context_action, context_presentation, media_playback_icon};

    #[test]
    fn timer_presentation_uses_stable_clock_digits_and_icon() {
        let presentation = context_presentation(
            &ContextCard::Timer {
                id: "timer-1".to_string(),
                label: "Tea".to_string(),
                remaining_seconds: 62,
                target_epoch: Some(1_800_000_062),
                completed: false,
                tier: ContextTier::Imminent,
            },
            1_800_000_000,
        );

        assert_eq!(presentation.icon_name, "alarm-symbolic");
        assert_eq!(presentation.text, "Tea · 1:02");
    }

    #[test]
    fn calendar_presentation_includes_location_and_relative_time() {
        let presentation = context_presentation(
            &ContextCard::Calendar {
                id: "event-1".to_string(),
                title: "Review".to_string(),
                location: Some("Lab".to_string()),
                start_epoch: 1_800_000_600,
                tier: ContextTier::Imminent,
            },
            1_800_000_000,
        );

        assert_eq!(presentation.icon_name, "appointment-soon-symbolic");
        assert_eq!(presentation.text, "Review — Lab · in 10m");
    }

    #[test]
    fn activity_presentation_uses_human_status_text() {
        let presentation = context_presentation(
            &ContextCard::Activity {
                id: "build-1".to_string(),
                label: "Cargo test".to_string(),
                cwd: PathBuf::from("/tmp/project"),
                status: ActivityStatus::Succeeded,
                started_at: 1_800_000_000,
                finished_at: Some(1_800_000_010),
            },
            1_800_000_010,
        );

        assert_eq!(presentation.icon_name, "system-run-symbolic");
        assert_eq!(presentation.text, "Cargo test · finished");
    }

    #[test]
    fn media_playback_icon_reflects_playing_state() {
        assert_eq!(
            media_playback_icon(&PlaybackStatus::Playing),
            "media-playback-pause-symbolic"
        );
        assert_eq!(
            media_playback_icon(&PlaybackStatus::Paused),
            "media-playback-start-symbolic"
        );
        assert_eq!(
            media_playback_icon(&PlaybackStatus::Stopped),
            "media-playback-start-symbolic"
        );
    }

    #[test]
    fn battery_card_opens_power_tab() {
        let action = context_action(&ContextCard::Battery {
            percent: 20,
            charging: false,
            tier: ContextTier::Imminent,
        });
        assert_eq!(
            action,
            Some(ContextAction::OpenControlCenter(DesktopContext::Power))
        );
    }

    #[test]
    fn calendar_and_timer_cards_open_clock_tab() {
        let calendar = context_action(&ContextCard::Calendar {
            id: "event-1".to_string(),
            title: "Review".to_string(),
            location: None,
            start_epoch: 1_800_000_600,
            tier: ContextTier::Imminent,
        });
        let timer = context_action(&ContextCard::Timer {
            id: "timer-1".to_string(),
            label: "Tea".to_string(),
            remaining_seconds: 62,
            target_epoch: Some(1_800_000_062),
            completed: false,
            tier: ContextTier::Imminent,
        });

        assert_eq!(
            calendar,
            Some(ContextAction::OpenControlCenter(DesktopContext::Clock))
        );
        assert_eq!(
            timer,
            Some(ContextAction::OpenControlCenter(DesktopContext::Clock))
        );
    }

    #[test]
    fn media_card_opens_audio_tab() {
        let action = context_action(&ContextCard::Media {
            player: "spotify".to_string(),
            status: crate::PlaybackStatus::Playing,
            title: Some("Song".to_string()),
            artist: None,
        });
        assert_eq!(
            action,
            Some(ContextAction::OpenControlCenter(DesktopContext::Audio))
        );
    }

    #[test]
    fn activity_card_has_no_click_action() {
        let action = context_action(&ContextCard::Activity {
            id: "build-1".to_string(),
            label: "Cargo test".to_string(),
            cwd: PathBuf::from("/tmp/project"),
            status: ActivityStatus::Running,
            started_at: 1_800_000_000,
            finished_at: None,
        });
        assert_eq!(action, None);
    }

    #[test]
    fn urgent_card_with_window_focuses_that_window() {
        let action = context_action(&ContextCard::Urgent {
            output: "eDP-1".to_string(),
            workspace: Some("2".to_string()),
            window_id: Some("42".to_string()),
            window_title: Some("Terminal".to_string()),
        });
        assert_eq!(
            action,
            Some(ContextAction::FocusWindow {
                output: "eDP-1".to_string(),
                window_id: "42".to_string(),
            })
        );
    }

    #[test]
    fn urgent_card_without_window_has_no_click_action() {
        let action = context_action(&ContextCard::Urgent {
            output: "eDP-1".to_string(),
            workspace: None,
            window_id: None,
            window_title: None,
        });
        assert_eq!(action, None);
    }
}
