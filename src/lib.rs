pub mod config;
pub mod connection;
pub mod credentials;
pub mod device;
pub mod logging;
pub mod midi;
pub mod playback;
pub mod protocol;
pub mod recording;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
