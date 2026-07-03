use std::env;

use url::Url;

use crate::error::ServiceError;

const VERSION_HELIUM: &str = "1.71.0";
const VERSION_VANILLA: &str = "1.71.0";
const CSUM_HELIUM: &str = "38252894162bf0cc9ed682669760922c17af67d9a1bd27b082997d732895afd0";
const CSUM_VANILLA: &str = "5107ce702293e110ce6cc6467a51e689e919eed4382650c354c1d66db2aacc3d";
const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8000";
const DEFAULT_HEALTHCHECK_URL: &str = "http://127.0.0.1:8000/healthz";

#[derive(Clone)]
pub struct Config {
    pub(crate) base_url: Url,
    pub(crate) use_helium_assets: bool,
    pub(crate) custom_assets_url: Option<Url>,
    pub(crate) custom_assets_checksum: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let base_url = env::var("UBO_PROXY_BASE_URL")
            .map_err(|_| "env UBO_PROXY_BASE_URL is missing".to_string())
            .and_then(|value| Url::parse(&value).map_err(|err| err.to_string()))?;
        let use_helium_assets = !env_bool("UBO_USE_ORIGINAL_UBLOCK_ASSETS");
        let custom_assets_url = match env::var("UBO_ASSETS_JSON_URL")
            .ok()
            .filter(|value| !value.is_empty())
        {
            Some(value) => Some(Url::parse(&value).map_err(|err| err.to_string())?),
            None => None,
        };
        let custom_assets_checksum = env::var("UBO_ASSETS_JSON_SHA256")
            .ok()
            .filter(|value| !value.is_empty());

        let config = Self {
            base_url,
            use_helium_assets,
            custom_assets_url,
            custom_assets_checksum,
        };
        config.validate()?;
        Ok(config)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if !self.use_helium_assets && self.custom_assets_checksum.is_some() {
            return Err(
                "USE_ORIGINAL_UBLOCK_ASSETS and UBO_ASSETS_JSON_* cannot be set at the same time"
                    .to_string(),
            );
        }

        if self.custom_assets_url.is_some() != self.custom_assets_checksum.is_some() {
            return Err(
                "one of UBO_ASSETS_JSON_{URL,SHA256} is defined, but otheris missing".to_string(),
            );
        }

        Ok(())
    }

    pub(crate) fn assets_url(&self) -> Result<Url, ServiceError> {
        if let Some(url) = &self.custom_assets_url {
            return Ok(url.clone());
        }

        let repo = if self.use_helium_assets {
            "imputnet/uBlock"
        } else {
            "gorhill/uBlock"
        };
        let version = if self.use_helium_assets {
            VERSION_HELIUM
        } else {
            VERSION_VANILLA
        };

        Url::parse(&format!(
            "https://raw.githubusercontent.com/{repo}/refs/tags/{version}/assets/assets.json"
        ))
        .map_err(ServiceError::internal)
    }

    pub(crate) fn file_checksum(&self) -> Result<String, ServiceError> {
        Ok(self.custom_assets_checksum.clone().unwrap_or_else(|| {
            if self.use_helium_assets {
                CSUM_HELIUM.to_string()
            } else {
                CSUM_VANILLA.to_string()
            }
        }))
    }
}

pub fn load_dotenv() {
    let _ = dotenvy::dotenv();
}

pub fn bind_addr() -> String {
    env_string("HELIUM_BIND_ADDR", DEFAULT_BIND_ADDR)
}

pub fn healthcheck_url() -> String {
    env_string("HELIUM_HEALTHCHECK_URL", DEFAULT_HEALTHCHECK_URL)
}

fn env_bool(name: &str) -> bool {
    let value = env::var(name).unwrap_or_default().to_lowercase();
    matches!(value.as_str(), "true" | "yes" | "on" | "t" | "y" | "1")
}

fn env_string(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_rejects_partial_custom_assets() {
        let config = Config {
            base_url: Url::parse("http://localhost:8000/").unwrap(),
            use_helium_assets: true,
            custom_assets_url: Some(Url::parse("http://localhost/assets.json").unwrap()),
            custom_assets_checksum: None,
        };

        assert!(config.validate().is_err());
    }
}
