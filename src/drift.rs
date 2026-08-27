use std::{io::Cursor, path::Path};

use bytes::Bytes;
use flate2::read::GzDecoder;

use crate::{config, dict, ubo::tags::sha256_hex, upstream};

const UPSTREAM_UBO_PINS_URL: &str =
    "https://raw.githubusercontent.com/imputnet/helium-services/main/svc/ubo/lib/assets-info.ts";
const HELIUM_DEPS_URL: &str = "https://raw.githubusercontent.com/imputnet/helium/main/deps.ini";
const UPSTREAM_BANGS_URL: &str =
    "https://raw.githubusercontent.com/imputnet/helium-services/main/svc/bangs/bangs.json";
const UBO_PINS_LIMIT: usize = 256 * 1024;
const UBO_ASSETS_LIMIT: usize = 4 * 1024 * 1024;
const BANGS_LIMIT: usize = 16 * 1024 * 1024;

pub async fn check_upstream_compatibility() -> Result<(), String> {
    let client = upstream::metadata_client()?;

    let pins = fetch(&client, UPSTREAM_UBO_PINS_URL, UBO_PINS_LIMIT, "drift_ubo").await?;
    let pins = std::str::from_utf8(&pins).map_err(|_| "uBO pin source is not UTF-8".to_string())?;
    validate_vanilla_ubo_pins(pins)?;

    let deps = fetch(
        &client,
        HELIUM_DEPS_URL,
        UBO_PINS_LIMIT,
        "drift_helium_deps",
    )
    .await?;
    let deps = std::str::from_utf8(&deps)
        .map_err(|_| "Helium dependency source is not UTF-8".to_string())?;
    validate_helium_ubo_version(deps)?;

    let assets_url = format!(
        "https://raw.githubusercontent.com/imputnet/uBlock/refs/tags/{}/assets/assets.json",
        config::VERSION_HELIUM
    );
    let assets = fetch(
        &client,
        &assets_url,
        UBO_ASSETS_LIMIT,
        "drift_helium_assets",
    )
    .await?;
    validate_helium_assets(&assets, config::CSUM_HELIUM)?;

    let upstream_bangs = fetch(&client, UPSTREAM_BANGS_URL, BANGS_LIMIT, "drift_bangs").await?;
    validate_bangs(include_bytes!("../assets/bangs.json"), &upstream_bangs)?;

    let dictionary = fetch(
        &client,
        dict::DEFAULT_TARBALL_URL,
        dict::TARBALL_LIMIT,
        "drift_dictionary",
    )
    .await?;
    validate_dictionary_archive(&dictionary)?;

    println!("compatibility pins match Helium and Chromium upstreams");
    Ok(())
}

async fn fetch(
    client: &reqwest::Client,
    url: &str,
    limit: usize,
    category: &'static str,
) -> Result<Bytes, String> {
    let response = upstream::checked_response(client.get(url).send().await, category)
        .await
        .map_err(|_| format!("failed to fetch {category}"))?;
    upstream::read_limited(response, limit, category)
        .await
        .map_err(|_| format!("failed to read {category}"))
}

fn validate_vanilla_ubo_pins(source: &str) -> Result<(), String> {
    for (name, expected) in [
        ("VERSION_VANILLA", config::VERSION_VANILLA),
        ("CSUM_VANILLA", config::CSUM_VANILLA),
    ] {
        let actual = typescript_string_constant(source, name)
            .ok_or_else(|| format!("upstream uBO pin {name} is missing"))?;
        if actual != expected {
            return Err(format!(
                "uBO pin drift: {name} is {expected} locally and {actual} upstream"
            ));
        }
    }
    Ok(())
}

fn validate_helium_ubo_version(source: &str) -> Result<(), String> {
    let actual = ini_section_value(source, "ublock_origin", "version")
        .ok_or_else(|| "Helium uBlock dependency version is missing".to_string())?;
    if actual != config::VERSION_HELIUM {
        return Err(format!(
            "uBO pin drift: VERSION_HELIUM is {} locally and {actual} upstream",
            config::VERSION_HELIUM
        ));
    }
    Ok(())
}

fn ini_section_value<'a>(source: &'a str, section: &str, key: &str) -> Option<&'a str> {
    let mut in_section = false;
    for line in source.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_section = &line[1..line.len() - 1] == section;
        } else if in_section
            && let Some((actual_key, value)) = line.split_once('=')
            && actual_key.trim() == key
        {
            return Some(value.trim());
        }
    }
    None
}

fn validate_helium_assets(source: &[u8], expected: &str) -> Result<(), String> {
    let actual = sha256_hex(source);
    if actual != expected {
        return Err(format!(
            "uBO pin drift: CSUM_HELIUM is {expected} locally and {actual} upstream"
        ));
    }
    Ok(())
}

fn typescript_string_constant<'a>(source: &'a str, name: &str) -> Option<&'a str> {
    let declaration = format!("const {name}");
    let remainder = source.split_once(&declaration)?.1;
    let value = remainder.split_once('=')?.1.trim_start();
    let quote = value.as_bytes().first().copied()?;
    if !matches!(quote, b'\'' | b'\"') {
        return None;
    }
    let value = &value[1..];
    let end = value.as_bytes().iter().position(|byte| *byte == quote)?;
    Some(&value[..end])
}

fn validate_bangs(local: &[u8], upstream: &[u8]) -> Result<(), String> {
    if local == upstream {
        Ok(())
    } else {
        Err("bangs drift: assets/bangs.json differs from Helium upstream".to_string())
    }
}

fn validate_dictionary_archive(archive: &[u8]) -> Result<(), String> {
    let decoder = GzDecoder::new(Cursor::new(archive));
    let mut archive = tar::Archive::new(decoder);
    let mut paths = Vec::new();
    for entry in archive.entries().map_err(|err| err.to_string())? {
        let entry = entry.map_err(|err| err.to_string())?;
        paths.push(entry.path().map_err(|err| err.to_string())?.into_owned());
    }
    validate_dictionary_paths(&paths)
}

fn validate_dictionary_paths(paths: &[impl AsRef<Path>]) -> Result<(), String> {
    if paths
        .iter()
        .any(|path| path.as_ref() == Path::new(dict::REQUIRED_DICTIONARY))
    {
        Ok(())
    } else {
        Err(format!(
            "dictionary drift: pinned archive is missing {}",
            dict::REQUIRED_DICTIONARY
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_fixtures_pass_all_drift_checks() {
        validate_vanilla_ubo_pins(include_str!("../tests/fixtures/ubo-pins-current.ts")).unwrap();
        validate_helium_ubo_version(include_str!("../tests/fixtures/helium-deps-current.ini"))
            .unwrap();
        let assets = b"current assets";
        validate_helium_assets(assets, &sha256_hex(assets)).unwrap();
        validate_bangs(
            include_bytes!("../tests/fixtures/bangs-current.json"),
            include_bytes!("../tests/fixtures/bangs-current.json"),
        )
        .unwrap();
        let paths = include_str!("../tests/fixtures/dictionary-files-current.txt")
            .lines()
            .collect::<Vec<_>>();
        validate_dictionary_paths(&paths).unwrap();
    }

    #[test]
    fn stale_fixtures_fail_each_drift_check() {
        assert!(
            validate_vanilla_ubo_pins(include_str!("../tests/fixtures/ubo-pins-stale.ts")).is_err()
        );
        assert!(
            validate_helium_ubo_version(include_str!("../tests/fixtures/helium-deps-stale.ini"))
                .is_err()
        );
        assert!(validate_helium_assets(b"stale assets", config::CSUM_HELIUM).is_err());
        assert!(
            validate_bangs(
                include_bytes!("../tests/fixtures/bangs-current.json"),
                include_bytes!("../tests/fixtures/bangs-stale.json"),
            )
            .is_err()
        );
        let paths = include_str!("../tests/fixtures/dictionary-files-stale.txt")
            .lines()
            .collect::<Vec<_>>();
        assert!(validate_dictionary_paths(&paths).is_err());
    }
}
