use ratatui::style::Color;

use crate::config::ThemeChoice;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Theme {
    pub background: Color,
    pub foreground: Color,
    pub muted: Color,
    pub accent: Color,
    pub user: Color,
    pub character: Color,
    pub greeting: Color,
    pub error: Color,
    pub selection: Color,
}

impl Theme {
    pub fn resolve(choice: ThemeChoice) -> Self {
        match choice {
            ThemeChoice::Light => Self::light(),
            ThemeChoice::Dark => Self::dark(),
            ThemeChoice::Auto => {
                if terminal_looks_light() {
                    Self::light()
                } else {
                    Self::dark()
                }
            }
        }
    }

    pub fn dark() -> Self {
        Self {
            background: Color::Rgb(18, 20, 26),
            foreground: Color::Rgb(224, 228, 238),
            muted: Color::Rgb(143, 151, 170),
            accent: Color::Rgb(121, 166, 255),
            user: Color::Rgb(118, 203, 179),
            character: Color::Rgb(232, 173, 111),
            greeting: Color::Rgb(190, 144, 255),
            error: Color::Rgb(255, 112, 112),
            selection: Color::Rgb(56, 69, 94),
        }
    }

    pub fn light() -> Self {
        Self {
            background: Color::Rgb(250, 250, 248),
            foreground: Color::Rgb(31, 35, 42),
            muted: Color::Rgb(95, 103, 116),
            accent: Color::Rgb(27, 89, 171),
            user: Color::Rgb(0, 112, 86),
            character: Color::Rgb(148, 70, 0),
            greeting: Color::Rgb(108, 57, 161),
            error: Color::Rgb(178, 31, 31),
            selection: Color::Rgb(215, 226, 244),
        }
    }
}

fn terminal_looks_light() -> bool {
    std::env::var("COLORFGBG")
        .ok()
        .and_then(|value| value.rsplit(';').next()?.parse::<u8>().ok())
        .is_some_and(|background| background >= 7 && background != 8)
}
