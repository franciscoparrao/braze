//! Loading [`ConfigOverrides`] from the on-disk JSON config file.

use std::path::Path;

use crate::error::ConfigError;
use crate::overrides::ConfigOverrides;

/// Read and parse the config file at `path` into [`ConfigOverrides`].
///
/// Returns `Ok(None)` if the file does not exist — that is not an error;
/// callers should fall back to defaults and/or other layers. Any other I/O
/// failure (permissions, etc.) or a file that exists but fails to parse as
/// JSON is returned as `Err`.
pub fn load_file(path: &Path) -> Result<Option<ConfigOverrides>, ConfigError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ConfigError::ReadFile {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let overrides: ConfigOverrides =
        serde_json::from_str(&contents).map_err(|source| ConfigError::InvalidJson {
            path: path.to_path_buf(),
            source,
        })?;

    Ok(Some(overrides))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_file_returns_none_when_missing() {
        let path = Path::new("/nonexistent/path/that/braze-config/tests/never-create.json");
        let result = load_file(path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_file_parses_valid_json() {
        let dir = std::env::temp_dir().join(format!(
            "braze-config-test-{}-{}",
            std::process::id(),
            "load_file_parses_valid_json"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(
            &path,
            r#"{"default_backend": "anthropic", "max_tokens": 8192}"#,
        )
        .unwrap();

        let overrides = load_file(&path).unwrap().unwrap();
        assert_eq!(overrides.default_backend.as_deref(), Some("anthropic"));
        assert_eq!(overrides.max_tokens, Some(8192));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_file_rejects_invalid_json() {
        let dir = std::env::temp_dir().join(format!(
            "braze-config-test-{}-{}",
            std::process::id(),
            "load_file_rejects_invalid_json"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, "{ not valid json").unwrap();

        let err = load_file(&path).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidJson { .. }));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
