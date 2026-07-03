//! Load and merge `braze` configuration from hardcoded defaults, the
//! on-disk config file, `BRAZE_*` environment variables, and (later, from
//! `braze-cli`) explicit CLI overrides.
//!
//! Priority order, lowest to highest:
//! 1. [`Config::default`] — hardcoded defaults.
//! 2. `~/.config/braze/config.json` (XDG-aware), if present.
//! 3. `BRAZE_*` environment variables.
//! 4. [`ConfigOverrides`] applied explicitly via [`Config::apply_overrides`]
//!    — the seam `braze-cli` (Fase 5) will use to layer parsed `clap`
//!    flags on top, without this crate knowing `clap` exists.
//!
//! A missing config file is not an error: defaults (possibly refined by
//! env vars) are used instead. Only a config file that exists but fails to
//! parse as JSON, or an env var with an invalid value, is an error.
//!
//! ```no_run
//! # fn main() -> Result<(), braze_config::ConfigError> {
//! let config = braze_config::Config::load()?;
//! println!("default backend: {}", config.default_backend);
//! # Ok(())
//! # }
//! ```

mod config;
mod error;
mod file;
mod overrides;
mod paths;

pub use config::{Config, McpServerConfigStub};
pub use error::ConfigError;
pub use overrides::ConfigOverrides;
pub use paths::{config_file_path, default_session_dir};
