use std::fs::File;

use simplelog::{ConfigBuilder, LevelFilter, WriteLogger};

/// Initialize file logging. The DLL runs inside the game's (Proton) working directory,
/// normally the `ELDEN RING/Game/` folder, so this writes `unseamless_coop.log` there.
/// The startup line records the actual cwd so the log can be located if Proton's cwd differs.
pub fn init() {
    let config = ConfigBuilder::new().set_time_format_rfc3339().build();

    if let Ok(file) = File::create("unseamless_coop.log") {
        let _ = WriteLogger::init(LevelFilter::Info, config, file);
    }

    // Record any worker-thread panic to the log. (Release builds use panic=abort, so the
    // process still exits after this runs; the point is leaving a trace.)
    std::panic::set_hook(Box::new(|info| {
        log::error!("PANIC: {info}");
    }));

    log::info!("unseamless-coop loaded");
    if let Ok(cwd) = std::env::current_dir() {
        log::info!("cwd = {}", cwd.display());
    }
}
