use serde::{Deserialize, Serialize};

use crate::{chunk::ChunkConfig, lighting::LightingEngineConfig};

/// Configuration for world and level-specific settings.
///
/// Currently, it includes chunk-related options; more settings may be added later.
#[derive(Deserialize, Serialize, Clone)]
pub struct LevelConfig {
    /// Configuration for chunk behaviour and management.
    pub chunk: ChunkConfig,
    /// Configuration for lighting engine propagation mode.
    #[serde(default)]
    pub lighting: LightingEngineConfig,
    /// Number of ticks between autosave checks. If 0, autosave is disabled.
    #[serde(default = "default_autosave_ticks")]
    pub autosave_ticks: u64,
    /// Additional worlds (hub, dungeon, afk). Empty by default — only the
    /// vanilla trio is loaded.
    #[serde(default)]
    pub extra: Vec<ExtraWorld>,
    // TODO: More options
}

const fn default_autosave_ticks() -> u64 {
    6000 // Default to 5 minutes at 20 TPS
}

/// An additional world loaded alongside the vanilla trio.
///
/// The dimension type is always overworld: `into_level` pushes nether/end
/// into the `DIM-1`/`DIM1` path, and such a world's folder name would
/// collide with the actual nether.
#[derive(Deserialize, Serialize, Clone)]
pub struct ExtraWorld {
    /// Folder name inside the world folder. This is also the world's
    /// identifier for plugins.
    pub name: String,
    /// `"void"` — empty space beyond generated chunks, `"vanilla"` — normal
    /// generation with the same seed as the main world.
    #[serde(default = "default_generator")]
    pub generator: String,
}

fn default_generator() -> String {
    "void".to_string()
}

impl Default for LevelConfig {
    fn default() -> Self {
        Self {
            chunk: ChunkConfig::default(),
            lighting: LightingEngineConfig::default(),
            autosave_ticks: default_autosave_ticks(),
            extra: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_level_config_enables_autosave() {
        assert_eq!(LevelConfig::default().autosave_ticks, 6000);
    }

    #[test]
    fn generated_config_round_trip_keeps_autosave_enabled() {
        let generated = toml::to_string(&LevelConfig::default()).unwrap();
        let reloaded: LevelConfig = toml::from_str(&generated).unwrap();
        assert_eq!(reloaded.autosave_ticks, 6000);
    }

    #[test]
    fn an_explicit_zero_still_disables_autosave() {
        let disabled = LevelConfig {
            autosave_ticks: 0,
            ..LevelConfig::default()
        };
        let generated = toml::to_string(&disabled).unwrap();
        let reloaded: LevelConfig = toml::from_str(&generated).unwrap();
        assert_eq!(reloaded.autosave_ticks, 0);
    }
}
