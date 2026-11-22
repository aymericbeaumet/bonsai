//! Pick one checkout when a command acts on a host or owner group.

use super::*;

pub(super) struct Choice {
    pub(super) cmd: Cmd,
    pub(super) args: String,
    pub(super) entries: Vec<(PathBuf, String)>,
    pub(super) cursor: usize,
}

impl Model {
    pub(super) fn choose_checkout(&mut self, cmd: Cmd, args: &str) -> Effect {
        let entries = scope::resolve(self)
            .projects
            .into_iter()
            .flat_map(|selection| {
                selection.worktrees.into_iter().filter_map(move |worktree| {
                    if worktree.missing && cmd != Cmd::Copy {
                        return None;
                    }
                    let label = format!(
                        "{} · {} · {}{}",
                        selection.project.name,
                        rows::branch_label(worktree),
                        worktree.path.display(),
                        if worktree.missing {
                            " · directory deleted"
                        } else {
                            ""
                        },
                    );
                    Some((worktree.path.clone(), label))
                })
            })
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return self.say("no available checkouts in this scope");
        }
        self.keys.reset();
        self.chooser = Some(Choice {
            cmd,
            args: args.to_string(),
            entries,
            cursor: 0,
        });
        Effect::None
    }

    pub(super) fn choice_key(&mut self, key: KeyEvent) -> Effect {
        if ctrl_letter(&key, 'c') {
            return Effect::Quit;
        }
        let input = self.keys.feed(key, &[]);
        let Some(choice) = self.chooser.as_mut() else {
            return Effect::None;
        };
        match input {
            keys::Input::Motion(motion, count) => {
                let page = usize::from(self.viewport).max(1);
                let times = count.unwrap_or(1);
                let last = choice.entries.len().saturating_sub(1);
                let top = choice.cursor.saturating_sub(page / 2);
                choice.cursor = match motion {
                    keys::Motion::Down | keys::Motion::ScrollDown | keys::Motion::NextGroup => {
                        choice.cursor.saturating_add(times)
                    }
                    keys::Motion::Up | keys::Motion::ScrollUp | keys::Motion::PreviousGroup => {
                        choice.cursor.saturating_sub(times)
                    }
                    keys::Motion::HalfDown => choice
                        .cursor
                        .saturating_add((page / 2).max(1).saturating_mul(times)),
                    keys::Motion::HalfUp => choice
                        .cursor
                        .saturating_sub((page / 2).max(1).saturating_mul(times)),
                    keys::Motion::PageDown => {
                        choice.cursor.saturating_add(page.saturating_mul(times))
                    }
                    keys::Motion::PageUp => {
                        choice.cursor.saturating_sub(page.saturating_mul(times))
                    }
                    keys::Motion::First => count.map_or(0, |line| line.saturating_sub(1)),
                    keys::Motion::Last => count.map_or(last, |line| line.saturating_sub(1)),
                    keys::Motion::ScreenTop => top,
                    keys::Motion::ScreenMiddle => top.saturating_add(page / 2),
                    keys::Motion::ScreenBottom => top.saturating_add(page.saturating_sub(1)),
                    keys::Motion::AlignTop
                    | keys::Motion::AlignMiddle
                    | keys::Motion::AlignBottom => choice.cursor,
                }
                .min(last);
            }
            keys::Input::Key(key, _) => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.chooser = None;
                    self.keys.reset();
                }
                KeyCode::Enter => {
                    let choice = self.chooser.take().expect("an open checkout chooser");
                    self.keys.reset();
                    let Some((path, _)) = choice.entries.get(choice.cursor) else {
                        return self.say("select a checkout first");
                    };
                    let Some(project) = self.project_id_for(path) else {
                        return self.say("that checkout is no longer available; refresh the list");
                    };
                    self.view = View::Detail { project };
                    self.filtering = false;
                    self.filter.clear();
                    self.rebuild();
                    self.reveal(path);
                    return self.command(choice.cmd, &choice.args);
                }
                _ => {}
            },
            keys::Input::Pair(_) | keys::Input::Pending => {}
        }
        Effect::None
    }
}

pub(super) fn draw(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, choice: &mut Choice) {
    use ratatui::widgets::{Block, Clear, List, ListItem, ListState};

    let width = area.width.saturating_sub(4).clamp(1, 120);
    let height = u16::try_from(choice.entries.len().saturating_add(2))
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(2))
        .max(1);
    let rect = ratatui::layout::Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    let command = COMMANDS
        .iter()
        .find(|spec| spec.cmd == choice.cmd)
        .map_or("command", |spec| spec.name);
    let block = Block::bordered()
        .border_style(Style::new().fg(Color::Cyan))
        .title(format!(" Choose checkout · {command} "));
    let inner = block.inner(rect);
    let items = choice
        .entries
        .iter()
        .map(|(_, label)| ListItem::new(truncate(label, usize::from(inner.width))))
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(choice.cursor));
    frame.render_widget(Clear, rect);
    frame.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(Style::new().bg(Color::Cyan).fg(Color::Black)),
        rect,
        &mut state,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hq::tests::{press, sample};

    fn model() -> Model {
        let mut snapshot = sample();
        snapshot.warnings.clear();
        let mut model = Model::from_snapshot(snapshot);
        model.grouping = Grouping::Hosts;
        model.rebuild();
        model.cursor = model
            .rows
            .iter()
            .position(|row| row.depth == rows::Depth::Host)
            .unwrap();
        model
    }

    #[test]
    fn checkout_chooser_preserves_arguments_and_scoped_paths() {
        let mut model = model();
        assert!(matches!(
            model.choose_checkout(Cmd::Resume, "session query"),
            Effect::None
        ));
        let choice = model.chooser.as_ref().unwrap();
        assert_eq!(choice.args, "session query");
        assert_eq!(choice.entries.len(), 3);
        assert!(
            choice
                .entries
                .iter()
                .any(|(path, _)| path == Path::new("/trees/other/feature"))
        );
        assert!(matches!(
            model.choice_key(press(KeyCode::Esc)),
            Effect::None
        ));
        assert!(model.chooser.is_none());
        assert!(matches!(model.view, View::List));
    }

    #[test]
    fn copy_can_choose_deleted_checkouts_but_launch_and_cd_cannot() {
        let mut model = model();
        model.snapshot.projects[0].worktrees[0].missing = true;
        for cmd in [Cmd::Cd, Cmd::Edit, Cmd::Start, Cmd::Resume, Cmd::Copy] {
            model.choose_checkout(cmd, "");
            let choice = model.chooser.as_ref().unwrap();
            assert_eq!(choice.entries.len(), if cmd == Cmd::Copy { 3 } else { 2 });
        }
    }

    #[test]
    fn choosing_a_checkout_replays_the_command_at_that_exact_path() {
        let mut model = model();
        model.choose_checkout(Cmd::Cd, "");
        model.choice_key(press(KeyCode::Char('G')));
        let expected = model
            .chooser
            .as_ref()
            .unwrap()
            .entries
            .last()
            .unwrap()
            .0
            .clone();
        let effect = model.choice_key(press(KeyCode::Enter));
        assert!(matches!(effect, Effect::Cd(path) if path == expected));
        assert!(model.chooser.is_none());
        assert_eq!(model.selected_path().as_ref(), Some(&expected));
        assert!(matches!(model.view, View::Detail { .. }));
    }
}
