use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub mod components;
pub mod favorites;
pub mod history;
pub mod leaderboard;
pub mod main_page;
pub mod playlists;
pub mod search;
pub mod settings;
pub mod sidebar;

pub(crate) fn is_song_activation_key(key: &KeyEvent) -> bool {
    key.modifiers == KeyModifiers::NONE
        && matches!(
            key.code,
            KeyCode::Enter | KeyCode::Char('\r') | KeyCode::Char('l')
        )
}

#[cfg(test)]
mod tests {
    use super::is_song_activation_key;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn song_lists_accept_enter_and_l_for_playback() {
        for code in [KeyCode::Enter, KeyCode::Char('\r'), KeyCode::Char('l')] {
            assert!(is_song_activation_key(&KeyEvent::new(
                code,
                KeyModifiers::NONE
            )));
        }
        assert!(!is_song_activation_key(&KeyEvent::new(
            KeyCode::Char('l'),
            KeyModifiers::CONTROL
        )));
    }
}
