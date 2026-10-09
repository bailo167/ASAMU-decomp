//! Runtime audio: sound cues, ambient sounds, narration + subtitles, music, gameplay sound events.
//!
//! Status: stub — owned by its workstream. Registered in `add_default_plugins`.

use bevy::prelude::*;

/// Runtime audio: sound cues, ambient sounds, narration + subtitles, music, gameplay sound events.
pub struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn build(&self, _app: &mut App) {}
}
