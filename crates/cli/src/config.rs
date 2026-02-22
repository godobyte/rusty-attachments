//! Configuration loading from `~/.deadline/config`.
//!
//! Reads the same INI-format config file as the Python `deadline` CLI,
//! ensuring interoperability between the two tools.

use std::path::PathBuf;

use configparser::ini::Ini;

use crate::commands::CliError;

/// Deadline Cloud CLI configuration.
pub struct CliConfig {
    ini: Ini,
    path: PathBuf,
}

impl CliConfig {
    /// Load config from the default path (`~/.deadline/config`).
    pub fn load() -> Result<Self, CliError> {
        let path: PathBuf = Self::default_path();
        let mut ini: Ini = Ini::new();
        if path.exists() {
            ini.load(path.to_str().unwrap_or_default())
                .map_err(|e| CliError::Config(format!("Failed to load config: {}", e)))?;
        }
        Ok(Self { ini, path })
    }

    /// Load config from a specific path.
    #[allow(dead_code)]
    pub fn load_from(path: PathBuf) -> Result<Self, CliError> {
        let mut ini: Ini = Ini::new();
        if path.exists() {
            ini.load(path.to_str().unwrap_or_default())
                .map_err(|e| CliError::Config(format!("Failed to load config: {}", e)))?;
        }
        Ok(Self { ini, path })
    }

    /// Get a setting by dotted key (e.g. `"defaults.farm_id"`).
    pub fn get(&self, key: &str) -> Option<String> {
        let (section, field) = key.split_once('.')?;
        self.ini.get(section, field)
    }

    /// Set a setting by dotted key and persist to disk.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), CliError> {
        let (section, field) = key
            .split_once('.')
            .ok_or_else(|| CliError::Config(format!("Invalid key format: {key}")))?;
        self.ini.set(section, field, Some(value.to_string()));
        self.save()
    }

    /// Return all settings as a sorted list of `(key, value)` pairs.
    pub fn all_settings(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        let map = self.ini.get_map_ref();
        let mut sections: Vec<&String> = map.keys().collect();
        sections.sort();
        for section in sections {
            if let Some(fields) = map.get(section) {
                let mut keys: Vec<&String> = fields.keys().collect();
                keys.sort();
                for field in keys {
                    if let Some(Some(val)) = fields.get(field) {
                        out.push((format!("{}.{}", section, field), val.clone()));
                    }
                }
            }
        }
        out
    }

    /// Get the cache directory for hash/S3 check caches.
    pub fn cache_dir(&self) -> PathBuf {
        #[cfg(target_os = "windows")]
        {
            std::env::var("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("C:\\ProgramData"))
                .join("deadline")
                .join("cache")
        }
        #[cfg(not(target_os = "windows"))]
        {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".deadline")
                .join("cache")
        }
    }

    /// Persist current config to disk.
    fn save(&self) -> Result<(), CliError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CliError::Config(format!("Cannot create config dir: {}", e)))?;
        }
        self.ini
            .write(self.path.to_str().unwrap_or_default())
            .map_err(|e| CliError::Config(format!("Cannot write config: {}", e)))?;
        Ok(())
    }

    /// Default config file path.
    fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".deadline")
            .join("config")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_load_missing_config() {
        let dir: TempDir = TempDir::new().unwrap();
        let path: PathBuf = dir.path().join("nonexistent.ini");
        let cfg: CliConfig = CliConfig::load_from(path).unwrap();
        assert!(cfg.get("defaults.farm_id").is_none());
    }

    #[test]
    fn test_set_and_get() {
        let dir: TempDir = TempDir::new().unwrap();
        let path: PathBuf = dir.path().join("config.ini");
        let mut cfg: CliConfig = CliConfig::load_from(path.clone()).unwrap();

        cfg.set("defaults.farm_id", "farm-abc").unwrap();
        assert_eq!(cfg.get("defaults.farm_id"), Some("farm-abc".to_string()));

        // Reload from disk to verify persistence
        let cfg2: CliConfig = CliConfig::load_from(path).unwrap();
        assert_eq!(cfg2.get("defaults.farm_id"), Some("farm-abc".to_string()));
    }

    #[test]
    fn test_all_settings() {
        let dir: TempDir = TempDir::new().unwrap();
        let path: PathBuf = dir.path().join("config.ini");
        let mut cfg: CliConfig = CliConfig::load_from(path).unwrap();

        cfg.set("defaults.farm_id", "farm-1").unwrap();
        cfg.set("defaults.queue_id", "queue-2").unwrap();
        cfg.set("settings.auto_accept", "true").unwrap();

        let all: Vec<(String, String)> = cfg.all_settings();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0], ("defaults.farm_id".to_string(), "farm-1".to_string()));
        assert_eq!(all[1], ("defaults.queue_id".to_string(), "queue-2".to_string()));
        assert_eq!(all[2], ("settings.auto_accept".to_string(), "true".to_string()));
    }

    #[test]
    fn test_invalid_key_format() {
        let dir: TempDir = TempDir::new().unwrap();
        let path: PathBuf = dir.path().join("config.ini");
        let mut cfg: CliConfig = CliConfig::load_from(path).unwrap();

        let result = cfg.set("no_dot_key", "value");
        assert!(result.is_err());
    }
}
