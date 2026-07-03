use url::Url;

use crate::error::ServiceError;

pub(crate) fn is_valid_http_url(value: &str) -> bool {
    Url::parse(value)
        .map(|url| matches!(url.scheme(), "http" | "https"))
        .unwrap_or(false)
}

pub(crate) fn filename_for_source(source_url: &str) -> Result<String, ServiceError> {
    let url = Url::parse(source_url).map_err(ServiceError::internal)?;
    let basename = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|value| !value.is_empty())
        .unwrap_or("filters.txt");

    if basename.ends_with(".txt") || basename.ends_with(".dat") {
        Ok(basename.to_string())
    } else {
        Ok("filters.txt".to_string())
    }
}

pub(crate) fn join_url(base: &Url, path: &str) -> Result<String, ServiceError> {
    base.join(path)
        .map(|url| url.to_string())
        .map_err(ServiceError::internal)
}

pub(crate) fn join_source_relative(base: &str, relative_path: &str) -> Option<String> {
    let mut url = Url::parse(base).ok()?.join(relative_path).ok()?;
    url.set_fragment(None);
    Some(url.to_string())
}

pub(crate) fn posix_dirname(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((dirname, _)) if !dirname.is_empty() => dirname.to_string(),
        _ => ".".to_string(),
    }
}

pub(crate) fn posix_join(base: &str, relative: &str) -> String {
    let combined = if base == "." || base.is_empty() {
        relative.trim_start_matches('/').to_string()
    } else {
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            relative.trim_start_matches('/')
        )
    };

    let mut parts = Vec::new();
    for part in combined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_include_paths_stay_under_parent() {
        assert_eq!(
            posix_join("easy/hash", "sub/list.txt"),
            "easy/hash/sub/list.txt"
        );
        assert_eq!(posix_join("easy/hash", "../other.txt"), "easy/other.txt");
    }
}
