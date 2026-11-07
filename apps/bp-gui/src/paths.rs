use std::sync::OnceLock;

use bp_engine::home::Home;

static HOME: OnceLock<Home> = OnceLock::new();

/// Stores the resolved home. `main` calls this once, first; later calls are ignored.
pub fn set_home(home: Home) {
    let _ = HOME.set(home);
}

/// The resolved home. Panics if `main` has not called [`set_home`].
pub fn home() -> &'static Home {
    HOME.get().expect("paths::set_home was not called")
}
