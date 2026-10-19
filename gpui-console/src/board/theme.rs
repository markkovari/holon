//! The colours the board is drawn in, dark and light.
//!
//! Every colour the view uses comes from here and nothing in the view names one, so the two themes
//! cannot drift apart and a third is a function. Status is the one thing colour carries on its own
//! (a person scanning a wall of cards reads green, blue, orange before they read a word), so each
//! status has its own pair: a tinted ground and the ink that goes on it.

use gpui::{rgb, rgba, Rgba};

use super::tree::Status;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Dark,
    Light,
}

impl Mode {
    pub fn toggled(self) -> Mode {
        match self {
            Mode::Dark => Mode::Light,
            Mode::Light => Mode::Dark,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Theme {
    pub mode: Mode,
    /// The window.
    pub bg: Rgba,
    pub card: Rgba,
    pub card_border: Rgba,
    pub card_hover: Rgba,
    pub text: Rgba,
    pub muted: Rgba,
    pub faint: Rgba,
    /// The connectors between a card and what it started.
    pub line: Rgba,
    /// The tile behind a model's glyph, and a tool's.
    pub tile: Rgba,
    pub tile_border: Rgba,
    pub popover: Rgba,
    pub popover_border: Rgba,
    pub shadow: Rgba,
    pub approve: Rgba,
    pub approve_ink: Rgba,
    pub deny: Rgba,
    pub deny_ink: Rgba,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            mode: Mode::Dark,
            bg: rgb(0x151517),
            card: rgb(0x1e1e21),
            card_border: rgb(0x2c2c31),
            card_hover: rgb(0x3b3b42),
            text: rgb(0xf4f4f5),
            muted: rgb(0x8d8d96),
            faint: rgb(0x5d5d66),
            line: rgb(0x3f7fcc),
            tile: rgb(0x2a2a2f),
            tile_border: rgb(0x38383e),
            popover: rgb(0x232326),
            popover_border: rgb(0x38383e),
            shadow: rgba(0x00000066),
            approve: rgb(0x1f6b4a),
            approve_ink: rgb(0xe8fff4),
            deny: rgb(0x3a2a2d),
            deny_ink: rgb(0xfca5a5),
        }
    }

    pub fn light() -> Self {
        Self {
            mode: Mode::Light,
            bg: rgb(0xfafafa),
            card: rgb(0xffffff),
            card_border: rgb(0xeaeaee),
            card_hover: rgb(0xc9c9d2),
            text: rgb(0x18181b),
            muted: rgb(0x71717a),
            faint: rgb(0xa1a1aa),
            line: rgb(0x93c5fd),
            tile: rgb(0xffffff),
            tile_border: rgb(0xe4e4e9),
            popover: rgb(0xffffff),
            popover_border: rgb(0xe4e4e9),
            shadow: rgba(0x18181b1a),
            approve: rgb(0x16a34a),
            approve_ink: rgb(0xffffff),
            deny: rgb(0xf4f4f5),
            deny_ink: rgb(0xb91c1c),
        }
    }

    pub fn of(mode: Mode) -> Self {
        match mode {
            Mode::Dark => Self::dark(),
            Mode::Light => Self::light(),
        }
    }

    /// The ground and the ink of a status badge.
    pub fn badge(&self, s: Status) -> (Rgba, Rgba) {
        let dark = self.mode == Mode::Dark;
        match (s, dark) {
            (Status::Running, true) => (rgb(0x14304d), rgb(0x6cb4ff)),
            (Status::Running, false) => (rgb(0xe0f0fe), rgb(0x0b73c9)),
            (Status::NeedsYou, true) => (rgb(0x452a12), rgb(0xff9d4d)),
            (Status::NeedsYou, false) => (rgb(0xffedd8), rgb(0xd4540c)),
            (Status::Done, true) => (rgb(0x133a2b), rgb(0x4fd68f)),
            (Status::Done, false) => (rgb(0xdcf7e5), rgb(0x15803d)),
            (Status::Failed, true) => (rgb(0x431a1f), rgb(0xf87878)),
            (Status::Failed, false) => (rgb(0xfde4e4), rgb(0xc42626)),
            (Status::Stopped, true) => (rgb(0x2d2d32), rgb(0x9d9da6)),
            (Status::Stopped, false) => (rgb(0xf0f0f3), rgb(0x6b6b75)),
        }
    }

    /// The colour a card's edge takes while it needs a person: the one thing on the board that is
    /// allowed to ask for attention.
    pub fn attention(&self) -> Rgba {
        self.badge(Status::NeedsYou).1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_status_has_a_distinct_ink_in_both_themes() {
        for t in [Theme::dark(), Theme::light()] {
            let inks: Vec<u32> =
                [Status::Running, Status::NeedsYou, Status::Done, Status::Failed, Status::Stopped]
                    .iter()
                    .map(|s| {
                        let c = t.badge(*s).1;
                        ((c.r * 255.0) as u32) << 16
                            | ((c.g * 255.0) as u32) << 8
                            | (c.b * 255.0) as u32
                    })
                    .collect();
            let mut unique = inks.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(unique.len(), inks.len(), "{:?}", t.mode);
        }
    }

    #[test]
    fn the_other_theme_is_the_other_theme() {
        assert_eq!(Theme::of(Mode::Dark.toggled()).mode, Mode::Light);
        assert_eq!(Mode::Light.toggled(), Mode::Dark);
    }
}
