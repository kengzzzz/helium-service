use std::env;

use url::Url;

#[derive(Clone)]
pub struct ExtensionProxyConfig {
    pub(crate) proxy_base_url: Option<Url>,
    pub(crate) hmac_secret: Option<Vec<u8>>,
}

impl ExtensionProxyConfig {
    pub fn from_env() -> Result<Self, String> {
        let proxy_base_url = match env::var("PROXY_BASE_URL")
            .ok()
            .filter(|value| !value.is_empty())
        {
            Some(value) => Some(Url::parse(&value).map_err(|err| err.to_string())?),
            None => {
                eprintln!("PROXY_BASE_URL is not set, CRX requests will not be proxied");
                None
            }
        };

        let hmac_secret = match env::var("HMAC_SECRET")
            .ok()
            .filter(|value| !value.is_empty())
        {
            Some(value) if value.len() >= 32 => Some(value.into_bytes()),
            _ => {
                eprintln!("HMAC_SECRET is not set or <32 chars, CRX requests will not be proxied");
                None
            }
        };

        Ok(Self {
            proxy_base_url,
            hmac_secret,
        })
    }

    pub(crate) fn proxying_enabled(&self) -> bool {
        self.proxy_base_url.is_some() && self.hmac_secret.is_some()
    }
}
