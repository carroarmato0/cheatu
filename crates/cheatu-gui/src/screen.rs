//! Which screen the main area shows, derived from state each frame.

use cheatu_inject::Engine;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Screen {
    /// No game: the game list.
    ChooseGame,
    /// An RPG Maker game was attached: offer RPG Maker mode or the scanner.
    RpgChoice,
    /// The guided steps (Advanced off).
    Guided,
    /// The full scanner (Advanced on).
    Scanner,
    /// The RPG Maker (JavaScript) screen.
    RpgMaker,
}

/// What the user chose for an RPG Maker game.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum RpgPick {
    #[default]
    Undecided,
    RpgMaker,
    Memory,
}

#[derive(Copy, Clone, Debug, Default)]
pub struct ScreenState {
    pub attached: bool,
    /// The attached game's engine, when it is an RPG Maker game.
    pub rpg: Option<Engine>,
    pub pick: RpgPick,
    pub advanced: bool,
    /// "RPG Maker game not running yet? Set it up…" was used.
    pub rpg_setup: bool,
}

pub fn screen(s: &ScreenState) -> Screen {
    if !s.attached {
        return if s.rpg_setup {
            Screen::RpgMaker
        } else {
            Screen::ChooseGame
        };
    }
    match (s.rpg.is_some(), s.pick) {
        (true, RpgPick::Undecided) => Screen::RpgChoice,
        (true, RpgPick::RpgMaker) => Screen::RpgMaker,
        _ if s.advanced => Screen::Scanner,
        _ => Screen::Guided,
    }
}

/// The guided steps' wizard runs only while its screen is shown.
pub fn wizard_runs(screen: Screen) -> bool {
    screen == Screen::Guided
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_selection_covers_every_state() {
        let base = ScreenState::default();
        assert_eq!(screen(&base), Screen::ChooseGame);
        assert_eq!(
            screen(&ScreenState {
                rpg_setup: true,
                ..base
            }),
            Screen::RpgMaker
        );
        // Advanced does not matter without a game.
        assert_eq!(
            screen(&ScreenState {
                advanced: true,
                ..base
            }),
            Screen::ChooseGame
        );

        let att = ScreenState {
            attached: true,
            ..base
        };
        assert_eq!(screen(&att), Screen::Guided);
        assert_eq!(
            screen(&ScreenState {
                advanced: true,
                ..att
            }),
            Screen::Scanner
        );
        // A finished RPG Maker setup does not hide an attached ordinary game.
        assert_eq!(
            screen(&ScreenState {
                rpg_setup: true,
                ..att
            }),
            Screen::Guided
        );

        let rpg = ScreenState {
            rpg: Some(Engine::RpgMakerMZ),
            ..att
        };
        assert_eq!(screen(&rpg), Screen::RpgChoice);
        assert_eq!(
            screen(&ScreenState {
                advanced: true,
                ..rpg
            }),
            Screen::RpgChoice
        );
        assert_eq!(
            screen(&ScreenState {
                pick: RpgPick::RpgMaker,
                ..rpg
            }),
            Screen::RpgMaker
        );
        assert_eq!(
            screen(&ScreenState {
                pick: RpgPick::Memory,
                ..rpg
            }),
            Screen::Guided
        );
        assert_eq!(
            screen(&ScreenState {
                pick: RpgPick::Memory,
                advanced: true,
                ..rpg
            }),
            Screen::Scanner
        );
    }

    #[test]
    fn wizard_runs_only_on_guided_screen() {
        for s in [
            Screen::ChooseGame,
            Screen::RpgChoice,
            Screen::Scanner,
            Screen::RpgMaker,
        ] {
            assert!(!wizard_runs(s), "{s:?}");
        }
        assert!(wizard_runs(Screen::Guided));
    }
}
