//! The daemon's own persistent state (D-07), independent of any SQLite index.
//!
//! Maps each transcript file path to its last-seen `(mtime, size)` and the
//! sha256 last archived from it. Lives at `config.state_dir()/watermark.json`
//! and is saved via temp-file-plus-rename so an interrupted save never leaves a
//! torn file.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;

/// Last-seen stat and archived sha for one transcript file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub mtime_secs: i64,
    pub mtime_nanos: u32,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct WatermarkFile {
    #[serde(default)]
    entries: BTreeMap<String, Entry>,
}

/// Loaded watermark state plus the path it persists to.
pub struct Watermark {
    path: PathBuf,
    file: WatermarkFile,
}

impl Watermark {
    /// Load from `state_dir/watermark.json`; an absent file is an empty watermark.
    pub fn load(config: &Config) -> Result<Self> {
        let path = config.state_dir().join("watermark.json");
        let file = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing watermark {}", path.display()))?,
            Err(e) if e.kind() == ErrorKind::NotFound => WatermarkFile::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Watermark { path, file })
    }

    /// The last-recorded entry for a transcript, if any.
    pub fn get(&self, transcript: &Path) -> Option<&Entry> {
        self.file.entries.get(&key(transcript))
    }

    /// Record (or replace) the entry for a transcript. Call `save` to persist.
    pub fn record(&mut self, transcript: &Path, entry: Entry) {
        self.file.entries.insert(key(transcript), entry);
    }

    /// Persist the watermark atomically (temp-file-plus-rename).
    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating state dir {}", parent.display()))?;
        }
        let json = serde_json::to_vec_pretty(&self.file).context("serializing watermark")?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &json).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("renaming watermark into {}", self.path.display()))?;
        Ok(())
    }
}

fn key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
