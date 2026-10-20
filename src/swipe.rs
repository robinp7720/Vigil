//! Live workspace-swipe progress from the compositor. GTK-free.
//!
//! Swipe samples arrive at frame rate, far faster than the UI's state loop
//! polls, so they skip `StateUpdate` entirely. The compositor thread publishes
//! the latest sample per output here, and the workspace strip reads it from its
//! own frame-clock tick.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceSwipe {
    pub from: String,
    /// The neighbour the swipe is heading to, if the compositor knows it.
    pub to: Option<String>,
    /// Signed fraction of one workspace step: negative heads to the previous
    /// workspace, positive to the next.
    pub progress: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SwipeEvent {
    Progress {
        output: String,
        swipe: WorkspaceSwipe,
    },
    End {
        output: String,
    },
}

type Waker = Box<dyn Fn() + Send + Sync>;

#[derive(Default)]
pub struct SwipeFeed {
    samples: Mutex<BTreeMap<String, WorkspaceSwipe>>,
    waker: Mutex<Option<Waker>>,
}

impl SwipeFeed {
    pub fn global() -> &'static SwipeFeed {
        static FEED: OnceLock<SwipeFeed> = OnceLock::new();
        FEED.get_or_init(SwipeFeed::default)
    }

    /// Called whenever a swipe starts on an output that had none, so idle
    /// consumers can resume ticking.
    pub fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        *self.waker.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(waker));
    }

    pub fn apply(&self, event: SwipeEvent) {
        let started = {
            let mut samples = self.samples.lock().unwrap_or_else(|e| e.into_inner());
            match event {
                SwipeEvent::Progress { output, swipe } => samples.insert(output, swipe).is_none(),
                // Consumers that saw the swipe are still ticking and notice the end.
                SwipeEvent::End { output } => {
                    samples.remove(&output);
                    false
                }
            }
        };
        if started
            && let Some(waker) = self
                .waker
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
        {
            waker();
        }
    }

    pub fn sample(&self, output: &str) -> Option<WorkspaceSwipe> {
        self.samples
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(output)
            .cloned()
    }

    /// Drops every in-flight swipe, e.g. when the compositor connection resets
    /// and an end event may have been lost.
    pub fn clear(&self) {
        self.samples
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

/// Parses the events published by `tools/hypr-swipe-events`.
pub fn parse_hyprland_swipe(event: &str, payload: &str) -> Option<SwipeEvent> {
    match event {
        "workspaceswipe" => {
            let mut parts = payload.splitn(4, ',');
            let output = parts.next().filter(|value| !value.is_empty())?;
            let from = parts.next().filter(|value| !value.is_empty())?;
            let to = parts.next()?;
            let progress = parts.next()?.trim().parse::<f64>().ok()?;
            Some(SwipeEvent::Progress {
                output: output.to_string(),
                swipe: WorkspaceSwipe {
                    from: from.to_string(),
                    to: (!to.is_empty()).then(|| to.to_string()),
                    progress: progress.clamp(-1.0, 1.0),
                },
            })
        }
        "workspaceswipeend" => Some(SwipeEvent::End {
            output: payload.trim().to_string(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn parses_progress_and_end_events() {
        assert_eq!(
            parse_hyprland_swipe("workspaceswipe", "DP-1,2,3,0.4200"),
            Some(SwipeEvent::Progress {
                output: "DP-1".to_string(),
                swipe: WorkspaceSwipe {
                    from: "2".to_string(),
                    to: Some("3".to_string()),
                    progress: 0.42,
                },
            })
        );
        assert_eq!(
            parse_hyprland_swipe("workspaceswipe", "DP-1,2,,0.0000"),
            Some(SwipeEvent::Progress {
                output: "DP-1".to_string(),
                swipe: WorkspaceSwipe {
                    from: "2".to_string(),
                    to: None,
                    progress: 0.0,
                },
            })
        );
        assert_eq!(
            parse_hyprland_swipe("workspaceswipeend", "DP-1"),
            Some(SwipeEvent::End {
                output: "DP-1".to_string()
            })
        );
    }

    #[test]
    fn rejects_malformed_and_unrelated_events() {
        assert_eq!(parse_hyprland_swipe("workspaceswipe", "DP-1,2"), None);
        assert_eq!(
            parse_hyprland_swipe("workspaceswipe", "DP-1,2,3,nope"),
            None
        );
        assert_eq!(parse_hyprland_swipe("workspaceswipe", ",2,3,0.1"), None);
        assert_eq!(parse_hyprland_swipe("workspacev2", "2,2"), None);
    }

    #[test]
    fn clamps_progress_to_one_step() {
        let Some(SwipeEvent::Progress { swipe, .. }) =
            parse_hyprland_swipe("workspaceswipe", "DP-1,2,1,-3.5")
        else {
            panic!("expected progress");
        };
        assert_eq!(swipe.progress, -1.0);
    }

    #[test]
    fn feed_wakes_only_when_a_swipe_starts() {
        let feed = SwipeFeed::default();
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        feed.set_waker(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });

        let progress = |value: f64| SwipeEvent::Progress {
            output: "DP-1".to_string(),
            swipe: WorkspaceSwipe {
                from: "1".to_string(),
                to: Some("2".to_string()),
                progress: value,
            },
        };
        feed.apply(progress(0.1));
        feed.apply(progress(0.2));
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        assert_eq!(feed.sample("DP-1").map(|swipe| swipe.progress), Some(0.2));

        feed.apply(SwipeEvent::End {
            output: "DP-1".to_string(),
        });
        assert_eq!(feed.sample("DP-1"), None);
        feed.apply(progress(0.3));
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
    }
}
