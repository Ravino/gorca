//! The shipped keymap presets: NVDA desktop (default), NVDA laptop and the Orca pair. They are
//! ordinary TOML in the same format as the user's file, embedded at build time.

use crate::settings::{Layout, Preset};

const NVDA_DESKTOP: &str = include_str!("../presets/nvda-desktop.toml");
const NVDA_LAPTOP: &str = include_str!("../presets/nvda-laptop.toml");
const ORCA_DESKTOP: &str = include_str!("../presets/orca-desktop.toml");
const ORCA_LAPTOP: &str = include_str!("../presets/orca-laptop.toml");

/// The preset texts `load` layers under the user's keymap. Tests can substitute their own.
#[derive(Copy, Clone, Debug)]
pub struct Presets {
    pub nvda_desktop: &'static str,
    pub nvda_laptop: &'static str,
    pub orca_desktop: &'static str,
    pub orca_laptop: &'static str,
}

impl Presets {
    pub const fn builtin() -> Self {
        Self {
            nvda_desktop: NVDA_DESKTOP,
            nvda_laptop: NVDA_LAPTOP,
            orca_desktop: ORCA_DESKTOP,
            orca_laptop: ORCA_LAPTOP,
        }
    }

    pub fn get(&self, preset: Preset, layout: Layout) -> &'static str {
        match (preset, layout) {
            (Preset::Nvda, Layout::Desktop) => self.nvda_desktop,
            (Preset::Nvda, Layout::Laptop) => self.nvda_laptop,
            (Preset::Orca, Layout::Desktop) => self.orca_desktop,
            (Preset::Orca, Layout::Laptop) => self.orca_laptop,
        }
    }
}

impl Default for Presets {
    fn default() -> Self {
        Self::builtin()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::ContextName;
    use crate::load::load;

    /// Every shipped preset loads with zero diagnostics (no KC-1 collisions, no KC-5 violations).
    #[test]
    fn every_preset_is_clean() {
        for preset in [Preset::Nvda, Preset::Orca] {
            for layout in [Layout::Desktop, Layout::Laptop] {
                let text = format!(
                    "[keyboard]\npreset = \"{}\"\nlayout = \"{}\"\n",
                    preset.name(),
                    layout.name()
                );
                let l = load(Some(&text), &Presets::builtin());
                assert!(
                    l.diags.is_empty(),
                    "{} {}: {:?}",
                    preset.name(),
                    layout.name(),
                    l.diags
                );
                assert!(
                    l.keymap
                        .table(ContextName::Global)
                        .is_some_and(|t| t.len() > 30)
                );
            }
        }
    }

    #[test]
    fn nvda_desktop_is_the_default() {
        let l = load(None, &Presets::builtin());
        let chord = crate::ChordSpec::parse("Insert+Tab").unwrap();
        let g = l.keymap.table(ContextName::Global).unwrap();
        assert_eq!(g[&chord].as_deref(), Some("info.where_am_i"));
    }

    #[test]
    fn the_orca_preset_binds_where_am_i_to_the_keypad_enter() {
        let l = load(Some("[keyboard]\npreset = \"orca\"\n"), &Presets::builtin());
        let chord = crate::ChordSpec::parse("KP_Enter").unwrap();
        assert_eq!(
            l.keymap.table(ContextName::Global).unwrap()[&chord].as_deref(),
            Some("info.where_am_i")
        );
    }
}
