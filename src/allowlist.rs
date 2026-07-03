use std::{collections::HashMap, sync::Arc};

use tokio::sync::Mutex;

#[derive(Default, Clone)]
pub(crate) struct Allowlist {
    inner: Arc<Mutex<AllowlistInner>>,
}

#[derive(Default)]
struct AllowlistInner {
    paths: HashMap<String, Vec<String>>,
    parents: HashMap<String, Vec<String>>,
}

impl Allowlist {
    pub(crate) async fn add_entries(&self, parent: &str, entries: HashMap<String, Vec<String>>) {
        let mut inner = self.inner.lock().await;
        if let Some(paths) = inner.parents.remove(parent) {
            for path in paths {
                inner.paths.remove(&path);
            }
        }

        inner
            .parents
            .insert(parent.to_string(), entries.keys().cloned().collect());
        for (path, urls) in entries {
            if inner.paths.contains_key(&path) {
                eprintln!("WARN: urls are already defined for {path}, skipping");
                continue;
            }
            inner.paths.insert(path, urls);
        }
    }

    pub(crate) async fn get_urls_for_path(&self, path: &str) -> Option<Vec<String>> {
        self.inner.lock().await.paths.get(path).cloned()
    }
}
