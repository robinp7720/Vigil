use super::*;
use crate::{FilesystemUsage, StorageState};

pub(super) struct StorageView {
    pub root: gtk::Box,
    status: gtk::Label,
    list: gtk::Box,
    previous: RefCell<Option<StorageState>>,
}

impl StorageView {
    pub fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.add_css_class("control-page");
        root.append(&section_intro(
            "drive-harddisk-symbolic",
            "Local storage",
            "Used and available space on this computer.",
        ));
        let status = gtk::Label::new(None);
        status.add_css_class("supporting-text");
        status.set_xalign(0.0);
        status.set_wrap(true);
        root.append(&status);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.append(&list);
        Self {
            root,
            status,
            list,
            previous: RefCell::new(None),
        }
    }

    pub fn update(&self, state: &StorageState, health: Option<&SourceHealth>) {
        let message = match health {
            Some(SourceHealth::Disconnected { message }) => {
                format!(
                    "Storage unavailable · {message}{}",
                    if state.filesystems.is_empty() {
                        ""
                    } else {
                        ". Showing the last snapshot."
                    }
                )
            }
            Some(SourceHealth::Stale { .. }) => {
                "Storage snapshot is out of date. Waiting for a refresh.".to_string()
            }
            _ if state.filesystems.is_empty() => "No local filesystems available.".to_string(),
            _ => "Local filesystems · refreshed every 30 seconds".to_string(),
        };
        self.status.set_label(&message);
        if self.previous.borrow().as_ref() == Some(state) {
            return;
        }
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for entry in &state.filesystems {
            self.list.append(&filesystem_row(entry));
        }
        *self.previous.borrow_mut() = Some(state.clone());
    }
}

fn filesystem_row(entry: &FilesystemUsage) -> gtk::Box {
    let gauge = metric_row(&entry.mount_point);
    update_metric(&gauge, Some(entry.used_percent));
    // Mount paths may be long; keep the panel within its existing width.
    if let Some(header) = gauge.root.first_child()
        && let Some(title) = header.first_child().and_downcast::<gtk::Label>()
    {
        title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        title.set_max_width_chars(32);
        title.set_tooltip_text(Some(&entry.mount_point));
    }
    let detail = gtk::Label::new(Some(&format!(
        "{} used of {} · {}",
        format_bytes(entry.used_bytes),
        format_bytes(entry.total_bytes),
        entry.filesystem
    )));
    detail.add_css_class("supporting-text");
    detail.set_xalign(0.0);
    detail.set_wrap(true);
    let free = gtk::Label::new(Some(&format!(
        "{} available",
        format_bytes(entry.available_bytes)
    )));
    free.add_css_class("metric-value");
    free.set_xalign(0.0);
    gauge.root.append(&detail);
    gauge.root.append(&free);
    gauge.root.set_tooltip_text(Some(&format!(
        "{}\n{}\n{} bytes used · {} bytes total · {} bytes available",
        entry.mount_point,
        entry.filesystem,
        entry.used_bytes,
        entry.total_bytes,
        entry.available_bytes
    )));
    gauge.root
}

pub(super) fn summary(state: &StorageState, health: Option<&SourceHealth>) -> String {
    if matches!(health, Some(SourceHealth::Disconnected { .. })) {
        return "Unavailable".to_string();
    }
    if matches!(health, Some(SourceHealth::Stale { .. })) {
        return "Out of date".to_string();
    }
    state
        .filesystems
        .iter()
        .max_by_key(|entry| entry.used_percent)
        .map(|entry| format!("{} · {}% used", entry.mount_point, entry.used_percent))
        .unwrap_or_else(|| "—".to_string())
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reports_available_space_in_binary_units_and_marks_stale_summaries() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1_500_000_000_000), "1.4 TiB");
        assert_eq!(
            summary(
                &StorageState::default(),
                Some(&SourceHealth::Stale { since_epoch: 0 })
            ),
            "Out of date"
        );
    }
}
