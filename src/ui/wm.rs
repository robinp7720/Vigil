use crate::{AppConfig, BarSnapshot, OutputState};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowGroupSpec {
    pub output_name: String,
    pub workspace_id: Option<String>,
    pub workspace_label: String,
    pub windows: Vec<WindowMenuItemSpec>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowMenuItemSpec {
    pub output_name: String,
    pub window_id: String,
    pub title: String,
    pub app_id: Option<String>,
    pub focused: bool,
}

/// Pick the output that hosts the full bar.
///
/// The configured primary wins only while it actually exists; otherwise the
/// first output is used. Falling back to the focused output is deliberately
/// avoided: focus follows the mouse, so the primary/reduced roles would swap
/// every time the cursor crosses a monitor and the bars would visibly jump.
pub fn select_primary_output(snapshot: &BarSnapshot, config: &AppConfig) -> Option<String> {
    if let Some(primary_output) = config
        .primary_output
        .as_ref()
        .filter(|name| snapshot.outputs.contains_key(name.as_str()))
    {
        return Some(primary_output.clone());
    }

    snapshot.outputs.keys().next().cloned()
}

pub fn title_for_output(output: &OutputState) -> String {
    if let Some(window) = output.focused_window.as_ref() {
        if !window.title.trim().is_empty() {
            return window.title.clone();
        }
        if let Some(app_id) = window
            .app_id
            .as_ref()
            .filter(|app_id| !app_id.trim().is_empty())
        {
            return app_id.clone();
        }
    }

    output
        .workspaces
        .iter()
        .find(|workspace| workspace.active)
        .or_else(|| output.workspaces.first())
        .map(|workspace| workspace.label.clone())
        .unwrap_or_else(|| output.name.clone())
}

pub fn window_groups(snapshot: &BarSnapshot) -> Vec<WindowGroupSpec> {
    let mut groups = Vec::new();

    for output in snapshot.outputs.values() {
        let focused_window_id = output
            .focused_window
            .as_ref()
            .map(|window| window.id.as_str());

        for workspace in &output.workspaces {
            let mut windows = output
                .windows
                .iter()
                .filter(|window| window.workspace_id.as_deref() == Some(workspace.id.as_str()))
                .map(|window| WindowMenuItemSpec {
                    output_name: output.name.clone(),
                    window_id: window.id.clone(),
                    title: window_title(window),
                    app_id: window.app_id.clone(),
                    focused: Some(window.id.as_str()) == focused_window_id,
                })
                .collect::<Vec<_>>();

            if windows.is_empty() {
                continue;
            }

            windows.sort_by(|left, right| {
                right
                    .focused
                    .cmp(&left.focused)
                    .then_with(|| left.title.cmp(&right.title))
                    .then_with(|| left.window_id.cmp(&right.window_id))
            });

            groups.push(WindowGroupSpec {
                output_name: output.name.clone(),
                workspace_id: Some(workspace.id.clone()),
                workspace_label: workspace.label.clone(),
                windows,
            });
        }

        let mut floating = output
            .windows
            .iter()
            .filter(|window| {
                window.workspace_id.as_ref().is_none_or(|workspace_id| {
                    !output
                        .workspaces
                        .iter()
                        .any(|workspace| workspace.id == *workspace_id)
                })
            })
            .map(|window| WindowMenuItemSpec {
                output_name: output.name.clone(),
                window_id: window.id.clone(),
                title: window_title(window),
                app_id: window.app_id.clone(),
                focused: Some(window.id.as_str()) == focused_window_id,
            })
            .collect::<Vec<_>>();

        if !floating.is_empty() {
            floating.sort_by(|left, right| {
                right
                    .focused
                    .cmp(&left.focused)
                    .then_with(|| left.title.cmp(&right.title))
                    .then_with(|| left.window_id.cmp(&right.window_id))
            });

            groups.push(WindowGroupSpec {
                output_name: output.name.clone(),
                workspace_id: None,
                workspace_label: output.name.clone(),
                windows: floating,
            });
        }
    }

    for group in &mut groups {
        group.windows.sort_by(|left, right| {
            right
                .focused
                .cmp(&left.focused)
                .then_with(|| left.title.cmp(&right.title))
                .then_with(|| left.window_id.cmp(&right.window_id))
        });
    }

    groups
}

fn window_title(window: &crate::WindowState) -> String {
    if !window.title.trim().is_empty() {
        window.title.clone()
    } else {
        window
            .app_id
            .clone()
            .unwrap_or_else(|| "Window".to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{AppConfig, BarSnapshot, OutputState, WindowState, WorkspaceState};

    use super::{select_primary_output, window_groups};

    #[test]
    fn primary_output_ignores_focus_and_missing_pins() {
        let snapshot = BarSnapshot {
            outputs: [
                ("DP-2".to_string(), OutputState::default()),
                ("DP-3".to_string(), OutputState::default()),
            ]
            .into_iter()
            .collect::<BTreeMap<_, _>>(),
            focused_output: Some("DP-2".to_string()),
            ..BarSnapshot::default()
        };

        let mut config = AppConfig::default();
        config.primary_output = Some("DP-3".to_string());
        assert_eq!(
            select_primary_output(&snapshot, &config).as_deref(),
            Some("DP-3")
        );

        // A stale pin must not make the full bar follow the mouse.
        config.primary_output = Some("DP-5".to_string());
        assert_eq!(
            select_primary_output(&snapshot, &config),
            Some("DP-2".to_string())
        );

        assert_eq!(
            select_primary_output(&BarSnapshot::default(), &config),
            None
        );
    }

    #[test]
    fn window_groups_include_current_windows_grouped_by_output_and_workspace() {
        let snapshot = BarSnapshot {
            outputs: [
                (
                    "DP-4".to_string(),
                    OutputState {
                        name: "DP-4".to_string(),
                        workspaces: vec![
                            workspace("1", "web", "DP-4", false),
                            workspace("2", "term", "DP-4", true),
                        ],
                        focused_window: Some(window("term-2", Some("2"), "cargo test", true)),
                        windows: vec![
                            window("term-1", Some("2"), "shell", false),
                            window("term-2", Some("2"), "cargo test", true),
                            window("browser-1", Some("1"), "docs", false),
                        ],
                        urgent: false,
                        changed_at: 0,
                    },
                ),
                (
                    "DP-5".to_string(),
                    OutputState {
                        name: "DP-5".to_string(),
                        workspaces: vec![workspace("3", "chat", "DP-5", true)],
                        focused_window: Some(window("chat-1", Some("3"), "Signal", true)),
                        windows: vec![window("chat-1", Some("3"), "Signal", true)],
                        urgent: false,
                        changed_at: 0,
                    },
                ),
            ]
            .into_iter()
            .collect::<BTreeMap<_, _>>(),
            ..BarSnapshot::default()
        };

        let groups = window_groups(&snapshot);

        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].output_name, "DP-4");
        assert_eq!(groups[0].workspace_label, "web");
        assert_eq!(
            groups[0]
                .windows
                .iter()
                .map(|window| window.window_id.as_str())
                .collect::<Vec<_>>(),
            vec!["browser-1"]
        );
        assert_eq!(groups[1].output_name, "DP-4");
        assert_eq!(groups[1].workspace_label, "term");
        assert_eq!(
            groups[1]
                .windows
                .iter()
                .map(|window| (window.window_id.as_str(), window.focused))
                .collect::<Vec<_>>(),
            vec![("term-2", true), ("term-1", false)]
        );
        assert_eq!(groups[2].output_name, "DP-5");
        assert_eq!(groups[2].workspace_label, "chat");
        assert_eq!(
            groups[2]
                .windows
                .iter()
                .map(|window| (window.window_id.as_str(), window.focused))
                .collect::<Vec<_>>(),
            vec![("chat-1", true)]
        );
    }

    fn workspace(id: &str, label: &str, output: &str, active: bool) -> WorkspaceState {
        WorkspaceState {
            id: id.to_string(),
            label: label.to_string(),
            output: output.to_string(),
            active,
            urgent: false,
            changed_at: 0,
        }
    }

    fn window(id: &str, workspace_id: Option<&str>, title: &str, _focused: bool) -> WindowState {
        WindowState {
            id: id.to_string(),
            app_id: None,
            title: title.to_string(),
            urgent: false,
            workspace_id: workspace_id.map(str::to_string),
            changed_at: 0,
        }
    }
}
