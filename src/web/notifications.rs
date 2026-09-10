use std::collections::{HashSet, VecDeque};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use serde_json::Value;

use crate::config::HqConfig;

const STATUS_SUFFIX: &str =
    " #[fg=yellow]HQ #{?@bonsai_hq_attention,#{@bonsai_hq_attention},0}#[default]";

pub struct AttentionNotifier {
    config: HqConfig,
    state: Mutex<NotificationState>,
}

#[derive(Default)]
struct NotificationState {
    initialized: bool,
    seen: HashSet<String>,
    seen_order: VecDeque<String>,
    count: usize,
    tmux_installed: bool,
}

impl AttentionNotifier {
    pub fn new(config: &HqConfig) -> Self {
        Self {
            config: config.clone(),
            state: Mutex::new(NotificationState::default()),
        }
    }

    pub fn observe(&self, snapshot: &Value) {
        if !self.config.notifications && !self.config.tmux_status {
            return;
        }
        let attention = snapshot["attention"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let fresh = new_attention(&mut state, &attention);
        if self.config.notifications {
            for item in fresh {
                let title = match item["kind"].as_str() {
                    Some("completed") => "Bonsai · Ready for review",
                    Some("error") => "Bonsai · Agent needs help",
                    _ => "Bonsai · Input needed",
                };
                let body = item["summary"]
                    .as_str()
                    .unwrap_or("An agent needs your attention")
                    .to_owned();
                std::thread::spawn(move || {
                    notify(title, &body);
                });
            }
        }
        if self.config.tmux_status && (!state.tmux_installed || state.count != attention.len()) {
            state.count = attention.len();
            if !state.tmux_installed
                && let Some(current) = tmux_status()
            {
                state.tmux_installed = current.ends_with(STATUS_SUFFIX)
                    || tmux_set("status-right", &format!("{current}{STATUS_SUFFIX}"));
            }
            if state.tmux_installed {
                tmux_set("@bonsai_hq_attention", &state.count.to_string());
            }
        }
    }
}

impl Drop for AttentionNotifier {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        if state.tmux_installed
            && let Some(current) = tmux_status()
            && let Some(original) = current.strip_suffix(STATUS_SUFFIX)
        {
            tmux_set("status-right", original);
        }
    }
}

fn new_attention(state: &mut NotificationState, items: &[Value]) -> Vec<Value> {
    let mut fresh = Vec::new();
    for item in items {
        let Some(id) = item["id"].as_str() else {
            continue;
        };
        if state.seen.insert(id.to_owned()) {
            state.seen_order.push_back(id.to_owned());
            if state.initialized {
                fresh.push(item.clone());
            }
        }
    }
    while state.seen_order.len() > 4096 {
        if let Some(old) = state.seen_order.pop_front() {
            state.seen.remove(&old);
        }
    }
    state.initialized = true;
    fresh
}

fn tmux_status() -> Option<String> {
    let output = Command::new("tmux")
        .args(["show-option", "-gqv", "status-right"])
        .output()
        .ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .trim_end_matches('\n')
            .to_owned()
    })
}

fn tmux_set(option: &str, value: &str) -> bool {
    Command::new("tmux")
        .args(["set-option", "-g", option, value])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn notify(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("osascript");
        command.args(["-e", "on run argv\ndisplay notification (item 1 of argv) with title (item 2 of argv)\nend run", body, title]);
        command
    };
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let mut command = {
        let mut command = Command::new("notify-send");
        command.args(["--app-name=Bonsai", "--", title, body]);
        command
    };
    #[cfg(not(target_os = "windows"))]
    {
        let _ = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = (title, body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn notifications_ignore_startup_and_refresh_but_notice_new_episodes() {
        let mut state = NotificationState::default();
        let old = json!({"id":"session:approval:1", "kind":"approval"});
        let new = json!({"id":"session:approval:2", "kind":"approval"});
        assert!(new_attention(&mut state, std::slice::from_ref(&old)).is_empty());
        assert!(new_attention(&mut state, std::slice::from_ref(&old)).is_empty());
        assert_eq!(
            new_attention(&mut state, &[old, new.clone()]),
            vec![new.clone()]
        );
        assert!(new_attention(&mut state, &[]).is_empty());
        assert!(new_attention(&mut state, &[new]).is_empty());
    }
}
