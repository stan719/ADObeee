//! Derived active colors are reused until selection, library identity or OS appearance changes.
use crate::{Palette, ThemeKind, ThemeLibrary, ThemePreference};
#[derive(Clone, Default)]
pub struct ActivePaletteCache {
    cached: Option<Cached>,
}
#[derive(Clone)]
struct Cached {
    preference: ThemePreference,
    library: ThemeLibrary,
    system: Option<ThemeKind>,
    fallback: ThemeKind,
    palette: Palette,
}
impl ActivePaletteCache {
    /// The boolean reports a cache miss, useful for deterministic performance regression tests.
    pub fn resolve(
        &mut self,
        preference: ThemePreference,
        library: &ThemeLibrary,
        system: Option<ThemeKind>,
        fallback: ThemeKind,
    ) -> (Palette, bool) {
        let system = if preference == ThemePreference::System { system } else { None };
        if let Some(cached) = &self.cached
            && cached.preference == preference
            && cached.library.same_snapshot(library)
            && cached.system == system
            && cached.fallback == fallback
        {
            return (cached.palette, false);
        }
        let palette = preference.palette(library, system, fallback);
        self.cached = Some(Cached { preference, library: library.clone(), system, fallback, palette });
        (palette, true)
    }
}
