use crate::{audio::AudioHandle, game::string::RCStr};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Sound {
    pub name: RCStr,
    pub audio: Option<AudioHandle>,
}
