//! Overlay đổi tên tab: bắt đầu, commit, và xử lý phím.

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::*;

pub(crate) fn start_rename(app: &mut App, tab_index: usize) {
    if let Some(tab) = app.tabs.get(tab_index) {
        app.rename = Some(RenameState {
            tab_index,
            buffer: tab.name.clone(),
        });
    }
}

pub(crate) fn commit_rename(app: &mut App) {
    if let Some(state) = app.rename.take() {
        let name = state.buffer.trim().to_string();
        if !name.is_empty()
            && let Some(tab) = app.tabs.get_mut(state.tab_index)
        {
            tab.name = name;
        }
    }
}

pub(crate) fn handle_rename_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.rename = None,
        KeyCode::Enter => commit_rename(app),
        KeyCode::Backspace => {
            if let Some(state) = &mut app.rename {
                state.buffer.pop();
            }
        }
        KeyCode::Char(c) => {
            if let Some(state) = &mut app.rename {
                state.buffer.push(c);
            }
        }
        _ => {}
    }
}
