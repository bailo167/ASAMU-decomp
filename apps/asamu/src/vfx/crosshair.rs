//! The HUD crosshair's state machine (`asamu.ASAMUHUD.ChangeCrosshair`,
//! `ToggleCrosshair`, `FadeCrosshair`, `ToggleStoryMode`; the Kismet action
//! `SeqAct_ChangeCrosshair`). Pure.
//!
//! The original shows the crosshair in a Scaleform movie with frame labels
//! per look (`normal`, `enabled`, `dot`, `story_normal`, `story_enabled`,
//! `hidden` and four fades); the movie's art is not ported, so
//! `crate::vfx` draws its own shapes for each [`CrosshairKind`]. The state
//! rules below are ported from local reading of the script (VFX_DECALS.md
//! §5); CONFIRMED (src) unless noted.
//!
//! Who calls what: the grapple gun calls `ChangeCrosshair(true, true)` when
//! its crosshair trace says "grapple-able", `ChangeCrosshair(false, true)`
//! when it says "not", and nothing when it hits a non-static-mesh surface in
//! range (G-TG-2); `EnableGrapple(false)` calls `ChangeCrosshair(false,
//! false)` (the dot); entering or leaving story mode calls
//! `ToggleStoryMode`; Kismet's `SeqAct_ToggleCrosshair` and the console
//! command `ToggleCrosshair` call `ToggleCrosshair(show, fade)`.
//! `SeqAct_ChangeCrosshair` (`bEnableCrosshair`, `bSuitCrosshair`, defaults
//! true/true) calls `ChangeCrosshair` but no shipped map uses it
//! (CONFIRMED, Kismet census).

/// The crosshair's current look (`CrosshairTypes`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CrosshairKind {
    /// `HIDDEN`.
    Hidden,
    /// `DOT` (no suit; the HUD's start value, CONFIRMED (cdo)).
    #[default]
    Dot,
    /// `DISABLED` (suit, nothing to grapple): movie label `normal`.
    Disabled,
    /// `ENABLED` (suit, target grapple-able): label `enabled`.
    Enabled,
    /// `STORY_DISABLED`: label `story_normal`.
    StoryDisabled,
    /// `STORY_ENABLED` (an interactable in range): label `story_enabled`.
    StoryEnabled,
}

/// A frame label the original sends to the crosshair movie.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrosshairFrame {
    /// `normal`.
    Normal,
    /// `enabled`.
    Enabled,
    /// `dot`.
    Dot,
    /// `story_normal`.
    StoryNormal,
    /// `story_enabled`.
    StoryEnabled,
    /// `hidden`.
    Hidden,
    /// `fadeout_normal`.
    FadeOutNormal,
    /// `fadein_normal`.
    FadeInNormal,
    /// `fadeout_story`.
    FadeOutStory,
    /// `fadein_story`.
    FadeInStory,
}

/// The HUD's crosshair state (`bShowCrosshair`, `bHUDStoryMode`,
/// `CurrentCrosshair`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CrosshairState {
    /// `bShowCrosshair` (default true, CONFIRMED (cdo)).
    pub show: bool,
    /// `bHUDStoryMode`.
    pub story: bool,
    /// `CurrentCrosshair`.
    pub current: CrosshairKind,
}

impl Default for CrosshairState {
    fn default() -> Self {
        Self {
            show: true,
            story: false,
            current: CrosshairKind::Dot,
        }
    }
}

impl CrosshairState {
    fn set(&mut self, kind: CrosshairKind, frame: CrosshairFrame) -> Option<CrosshairFrame> {
        if self.current == kind {
            return None;
        }
        self.current = kind;
        Some(frame)
    }

    /// `ChangeCrosshair(enable, suit)`: the frame sent, if any. Ignored
    /// while the crosshair is not shown; in story mode only the suit
    /// variants exist (`enable` without `suit` changes nothing there).
    pub fn change(&mut self, enable: bool, suit: bool) -> Option<CrosshairFrame> {
        if !self.show {
            return None;
        }
        if self.story {
            return match (enable, suit) {
                (false, true) => {
                    self.set(CrosshairKind::StoryDisabled, CrosshairFrame::StoryNormal)
                }
                (true, true) => self.set(CrosshairKind::StoryEnabled, CrosshairFrame::StoryEnabled),
                _ => None,
            };
        }
        match (enable, suit) {
            (false, true) => self.set(CrosshairKind::Disabled, CrosshairFrame::Normal),
            (true, true) => self.set(CrosshairKind::Enabled, CrosshairFrame::Enabled),
            _ => self.set(CrosshairKind::Dot, CrosshairFrame::Dot),
        }
    }

    /// `ToggleCrosshair(show, fade)`: the frames sent, in order. With `fade`
    /// the fade animation plays (unless already in that state) and the
    /// current look is kept; without, hiding sends `hidden`, and both then
    /// call `ChangeCrosshair(show, show)`.
    pub fn toggle(&mut self, show: bool, fade: bool) -> Vec<CrosshairFrame> {
        let mut frames = Vec::new();
        if fade && show != self.show {
            frames.push(match (show, self.story) {
                (true, true) => CrosshairFrame::FadeInStory,
                (true, false) => CrosshairFrame::FadeInNormal,
                (false, true) => CrosshairFrame::FadeOutStory,
                (false, false) => CrosshairFrame::FadeOutNormal,
            });
        }
        self.show = show;
        if !fade {
            if !show {
                self.current = CrosshairKind::Hidden;
                frames.push(CrosshairFrame::Hidden);
            }
            frames.extend(self.change(show, show));
        }
        frames
    }

    /// `ToggleStoryMode(on)` of the HUD: resets the look to hidden and calls
    /// `ChangeCrosshair(false, true)` (story or suit "not in range").
    pub fn set_story_mode(&mut self, on: bool) -> Option<CrosshairFrame> {
        self.story = on;
        self.current = CrosshairKind::Hidden;
        self.change(false, true)
    }

    /// Whether anything is drawn (shown, and not the hidden look).
    #[must_use]
    pub fn visible(&self) -> bool {
        self.show && self.current != CrosshairKind::Hidden
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gun_updates_switch_between_the_suit_looks() {
        let mut c = CrosshairState::default();
        assert_eq!(c.current, CrosshairKind::Dot);
        assert_eq!(c.change(false, true), Some(CrosshairFrame::Normal));
        assert_eq!(c.change(false, true), None, "same look: no frame");
        assert_eq!(c.change(true, true), Some(CrosshairFrame::Enabled));
        assert_eq!(c.current, CrosshairKind::Enabled);
        // EnableGrapple(false): the dot.
        assert_eq!(c.change(false, false), Some(CrosshairFrame::Dot));
    }

    #[test]
    fn story_mode_has_only_suit_variants() {
        let mut c = CrosshairState::default();
        assert_eq!(c.set_story_mode(true), Some(CrosshairFrame::StoryNormal));
        assert_eq!(c.change(true, true), Some(CrosshairFrame::StoryEnabled));
        assert_eq!(c.change(false, false), None);
        assert_eq!(c.current, CrosshairKind::StoryEnabled);
        assert_eq!(c.set_story_mode(false), Some(CrosshairFrame::Normal));
        assert_eq!(c.current, CrosshairKind::Disabled);
    }

    #[test]
    fn toggling_hides_and_restores() {
        let mut c = CrosshairState::default();
        c.change(true, true);
        assert_eq!(c.toggle(false, false), vec![CrosshairFrame::Hidden]);
        assert!(!c.visible());
        // Hidden: gun updates are ignored.
        assert_eq!(c.change(true, true), None);
        assert_eq!(c.toggle(true, false), vec![CrosshairFrame::Enabled]);
        assert!(c.visible());
    }

    #[test]
    fn fading_keeps_the_look_and_skips_redundant_fades() {
        let mut c = CrosshairState::default();
        c.change(false, true);
        assert_eq!(c.toggle(false, true), vec![CrosshairFrame::FadeOutNormal]);
        assert!(!c.show);
        assert_eq!(c.current, CrosshairKind::Disabled);
        assert_eq!(c.toggle(false, true), Vec::new());
        c.story = true;
        assert_eq!(c.toggle(true, true), vec![CrosshairFrame::FadeInStory]);
        assert!(c.visible());
    }
}
