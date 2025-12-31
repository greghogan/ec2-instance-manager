use anyhow::Result;
use directories::UserDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AppConfig {
    pub filter: Option<String>,
    pub t_family_credit: Option<String>,
    pub default_instance_type: Option<String>,
    pub refresh_interval_seconds: Option<u64>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            filter: None,
            t_family_credit: Some("standard".to_string()),
            default_instance_type: Some("t3a.nano".to_string()),
            refresh_interval_seconds: Some(15),
        }
    }
}

impl AppConfig {
    pub async fn load() -> Result<Self> {
        let path = Self::get_config_path()?;
        if path.exists() {
            let content = fs::read_to_string(&path).await?;
            let mut config: AppConfig = toml::from_str(&content)?;
            // Apply defaults for missing fields
            let defaults = AppConfig::default();
            if config.t_family_credit.is_none() {
                config.t_family_credit = defaults.t_family_credit;
            }
            if config.default_instance_type.is_none() {
                config.default_instance_type = defaults.default_instance_type;
            }
            if config.refresh_interval_seconds.is_none() {
                config.refresh_interval_seconds = defaults.refresh_interval_seconds;
            }
            Ok(config)
        } else {
            Ok(Self::default())
        }
    }

    pub async fn save(&self) -> Result<()> {
        let path = Self::get_config_path()?;
        let content = toml::to_string_pretty(self)?;
        fs::write(path, content).await?;
        Ok(())
    }

    fn get_config_path() -> Result<PathBuf> {
        if let Some(user_dirs) = UserDirs::new() {
            Ok(user_dirs.home_dir().join(".ec2-instance-manager.toml"))
        } else {
            anyhow::bail!("Could not determine home directory")
        }
    }
}
