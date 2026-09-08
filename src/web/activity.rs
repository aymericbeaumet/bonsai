use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

#[derive(Default, Serialize)]
struct Activity {
    terminals: Vec<Value>,
    tmux: Vec<Value>,
}

pub fn annotate(mut state: Value) -> Value {
    let paths: Vec<_> = state["projects"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|project| project["worktrees"].as_array().into_iter().flatten())
        .filter_map(|tree| tree["path"].as_str())
        .map(|path| {
            (
                path.to_owned(),
                crate::paths::canonicalize_or_self(Path::new(path)),
            )
        })
        .collect();
    let mut activities: BTreeMap<_, Activity> = paths
        .iter()
        .map(|(path, _)| (path.clone(), Activity::default()))
        .collect();

    for terminal in state["terminals"].as_array().into_iter().flatten() {
        // Tmux panes describe the underlying activity; its HQ client is only
        // an attachment and must not appear as another running shell.
        if terminal["kind"] == "tmux" {
            continue;
        }
        if let Some(path) = owner(&terminal["path"], &paths) {
            activities.get_mut(path).unwrap().terminals.push(json!({
                "id": terminal["id"], "title": terminal["title"],
                "kind": terminal["kind"], "exited": terminal["exited"]
            }));
        }
    }
    for pane in state["tmux"]["panes"].as_array().into_iter().flatten() {
        if let Some(path) = owner(&pane["path"], &paths) {
            activities.get_mut(path).unwrap().tmux.push(json!({
                "session": pane["session"], "window": pane["windowId"],
                "windowName": pane["windowName"], "windowIndex": pane["windowIndex"],
                "pane": pane["id"], "command": pane["command"],
                "active": pane["active"] == true && pane["windowActive"] == true
            }));
        }
    }
    if let Some(projects) = state["projects"].as_array_mut() {
        for project in projects {
            if let Some(trees) = project["worktrees"].as_array_mut() {
                for tree in trees {
                    if let Some(activity) =
                        tree["path"].as_str().and_then(|path| activities.get(path))
                    {
                        tree["activity"] =
                            serde_json::to_value(activity).expect("activity is serializable");
                    }
                }
            }
        }
    }
    state
}

fn owner<'a>(path: &Value, worktrees: &'a [(String, PathBuf)]) -> Option<&'a str> {
    let path = path.as_str().filter(|path| !path.is_empty())?;
    let path = crate::paths::canonicalize_or_self(Path::new(path));
    worktrees
        .iter()
        .filter(|(_, checkout)| path.starts_with(checkout))
        .max_by_key(|(_, checkout)| checkout.components().count())
        .map(|(original, _)| original.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> Value {
        json!({
            "projects": [{"id": "project", "worktrees": [
                {"path": "/projects/main"},
                {"path": "/projects/main/nested"},
                {"path": "/projects/idle"}
            ]}],
            "terminals": [],
            "tmux": {"available": true, "sessions": [], "panes": []}
        })
    }

    fn pane(id: &str, path: &str, window_active: bool) -> Value {
        json!({"id": id, "session": "development", "windowId": "@4",
            "windowIndex": 2, "windowName": "editor", "path": path,
            "command": "vim", "active": true, "windowActive": window_active})
    }

    #[test]
    fn every_worktree_remains_visible_without_tmux_or_terminals() {
        let mut input = state();
        input["tmux"] = json!({"available": false, "sessions": [], "panes": []});
        let result = annotate(input);
        let trees = result["projects"][0]["worktrees"].as_array().unwrap();
        assert_eq!(trees.len(), 3);
        for tree in trees {
            assert_eq!(tree["activity"], json!({"terminals": [], "tmux": []}));
        }
    }

    #[test]
    fn panes_in_all_windows_match_the_deepest_worktree_even_from_subdirectories() {
        let mut input = state();
        input["tmux"]["panes"] = json!([
            pane("%2", "/projects/main/src", true),
            pane("%3", "/projects/main/nested/lib", false),
            pane("%4", "/projects/main-other", true),
            pane("%5", "/unrelated", true)
        ]);
        let result = annotate(input);
        let trees = &result["projects"][0]["worktrees"];
        assert_eq!(trees[0]["activity"]["tmux"].as_array().unwrap().len(), 1);
        assert_eq!(trees[0]["activity"]["tmux"][0]["pane"], "%2");
        assert_eq!(trees[0]["activity"]["tmux"][0]["active"], true);
        assert_eq!(trees[1]["activity"]["tmux"].as_array().unwrap().len(), 1);
        assert_eq!(trees[1]["activity"]["tmux"][0]["pane"], "%3");
        assert_eq!(trees[1]["activity"]["tmux"][0]["window"], "@4");
        assert_eq!(trees[1]["activity"]["tmux"][0]["active"], false);
        assert!(trees[2]["activity"]["tmux"].as_array().unwrap().is_empty());
    }

    #[test]
    fn hq_shells_and_completed_commands_keep_their_worktree_without_counting_tmux_clients_twice() {
        let mut input = state();
        input["terminals"] = json!([
            {"id": "shell", "path": "/projects/main/src", "title": "Shell", "kind": "shell", "exited": false},
            {"id": "command", "path": "/projects/main", "title": "bonsai list", "kind": "command", "exited": true},
            {"id": "client", "path": "/projects/main", "title": "tmux", "kind": "tmux", "exited": false}
        ]);
        let result = annotate(input);
        let activity = &result["projects"][0]["worktrees"][0]["activity"];
        let terminals = activity["terminals"].as_array().unwrap();
        assert_eq!(terminals.len(), 2);
        assert_eq!(terminals[0]["id"], "shell");
        assert_eq!(terminals[1]["exited"], true);
        assert!(activity["tmux"].as_array().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_pane_directories_match_the_canonical_worktree() {
        let directory = tempfile::tempdir().unwrap();
        let checkout = directory.path().join("checkout");
        std::fs::create_dir_all(checkout.join("src")).unwrap();
        let alias = directory.path().join("alias");
        std::os::unix::fs::symlink(&checkout, &alias).unwrap();
        let mut input = state();
        input["projects"][0]["worktrees"][0]["path"] = json!(checkout);
        input["tmux"]["panes"] = json!([pane("%0", alias.join("src").to_str().unwrap(), true)]);
        let result = annotate(input);
        assert_eq!(
            result["projects"][0]["worktrees"][0]["activity"]["tmux"][0]["pane"],
            "%0"
        );
    }
}
