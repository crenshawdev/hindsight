//! Configuration: a TOML file at an XDG path holding `base_dir` and daemon knobs.
//!
//! The daemon, CLI, and hooks all read the same file (D-06). `base_dir` is
//! required and is never guessed - refusing to invent a default under the data
//! volume root is the ARC-02 guard.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

fn default_idle_timeout_secs() -> u64 {
    900
}

fn default_ollama_url() -> String {
    "http://127.0.0.1:11434".to_string()
}
fn default_embed_model() -> String {
    "qwen3-embedding:8b".to_string()
}
fn default_keep_alive() -> String {
    "5m".to_string()
}

/// The `[embed]` knobs (D-01, ADR 0013). Every field carries a serde default so
/// an absent `[embed]` table (or an absent field) yields the shipped defaults and
/// no config edit is required to run `hindsight embed`. Embedding runs
/// unconditionally on the GPU (ADR 0013), so there are no GPU-scheduling knobs.
#[derive(Debug, Clone, Deserialize)]
pub struct EmbedConfig {
    /// Ollama base URL; `/api/embed` is appended (D-01).
    #[serde(default = "default_ollama_url")]
    pub ollama_url: String,
    /// The embedding model requested (D-01).
    #[serde(default = "default_embed_model")]
    pub model: String,
    /// Ollama `keep_alive`: stays warm across a drain, unloads after (ADR 0004).
    #[serde(default = "default_keep_alive")]
    pub keep_alive: String,
}

impl Default for EmbedConfig {
    fn default() -> Self {
        Self {
            ollama_url: default_ollama_url(),
            model: default_embed_model(),
            keep_alive: default_keep_alive(),
        }
    }
}

/// Parsed `config.toml`. Shared by every subcommand.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Full path to the Hindsight data directory under the backed-up volume.
    /// Required; never defaulted (ARC-02 forbids guessing a volume-root path).
    pub base_dir: PathBuf,

    /// Daemon self-terminates after this many idle seconds with no poke.
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,

    /// Embedding knobs (D-01, ADR 0013). An absent `[embed]` table yields all
    /// defaults so no config edit is required to run `hindsight embed`.
    #[serde(default)]
    pub embed: EmbedConfig,
}

impl Config {
    /// Resolve the config file path: `$XDG_CONFIG_HOME/hindsight/config.toml`
    /// else `~/.config/hindsight/config.toml`.
    pub fn config_path() -> Result<PathBuf> {
        let dir = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(x) if !x.is_empty() => PathBuf::from(x),
            _ => {
                let home = std::env::var_os("HOME")
                    .filter(|h| !h.is_empty())
                    .ok_or_else(|| anyhow!("neither XDG_CONFIG_HOME nor HOME is set"))?;
                PathBuf::from(home).join(".config")
            }
        };
        Ok(dir.join("hindsight").join("config.toml"))
    }

    /// Read and validate the config from its XDG path.
    pub fn load() -> Result<Config> {
        let path = Self::config_path()?;
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "failed to read config at {} (create it with a `base_dir` key)",
                path.display()
            )
        })?;
        Self::from_toml_str(&text)
            .with_context(|| format!("invalid config at {}", path.display()))
    }

    /// Parse and validate config from a TOML string. Split out for testing.
    pub fn from_toml_str(s: &str) -> Result<Config> {
        let cfg: Config = toml::from_str(s).context("failed to parse config TOML")?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// ARC-02: refuse a `base_dir` that is a filesystem root or has no parent.
    fn validate(&self) -> Result<()> {
        if self.base_dir.as_os_str().is_empty() {
            bail!("base_dir is empty");
        }
        if self.base_dir.parent().is_none() {
            bail!(
                "base_dir {} is a filesystem root; it must be a subdirectory under the data volume (ARC-02)",
                self.base_dir.display()
            );
        }
        Ok(())
    }

    /// The verbatim archive lives under `base_dir/archive`.
    pub fn archive_dir(&self) -> PathBuf {
        self.base_dir.join("archive")
    }

    /// The daemon's own persistent state lives under `base_dir/state`.
    pub fn state_dir(&self) -> PathBuf {
        self.base_dir.join("state")
    }

    /// The rebuildable SQLite index lives under `base_dir/index` (D-09, ARC-02:
    /// never the volume root). Phase 6 (query) and Phase 7 (backfill and cutover)
    /// open this same path.
    pub fn index_dir(&self) -> PathBuf {
        self.base_dir.join("index")
    }

    /// The single SQLite database file: `base_dir/index/hindsight.db`.
    pub fn db_path(&self) -> PathBuf {
        self.index_dir().join("hindsight.db")
    }

    /// Map a transcript path to its archive coordinates.
    ///
    /// The mapping is taken from the path *relative to `sweep_root/projects`*:
    /// the first segment is `<project>`, the second is `<session-id>`, and any
    /// remaining segments form `<sub-path>` (empty for a top-level transcript).
    /// A trailing `.jsonl` is stripped from the final component so a nested
    /// subagent transcript keeps its sub-path (e.g. `subagents/agent-<id>`) and
    /// stays grouped under its real project/session.
    ///
    /// Segments are sanitized (no `.`, `..`, empty, or embedded separator) and
    /// the resolved output is verified to sit under `archive_dir()` so ARC-02
    /// holds as a runtime guard on both the sweep and PreCompact write paths.
    pub fn archive_key(
        &self,
        sweep_root: &Path,
        source_path: &Path,
    ) -> Result<(String, String, String)> {
        let projects_root = sweep_root.join("projects");
        let rel = source_path.strip_prefix(&projects_root).map_err(|_| {
            anyhow!(
                "transcript {} is not under {}",
                source_path.display(),
                projects_root.display()
            )
        })?;
        let rel_str = rel
            .to_str()
            .ok_or_else(|| anyhow!("non-UTF-8 transcript path {}", rel.display()))?;
        let rel_str = rel_str.strip_suffix(".jsonl").unwrap_or(rel_str);

        let segments: Vec<&str> = rel_str.split('/').collect();
        for seg in &segments {
            if seg.is_empty() || *seg == "." || *seg == ".." || seg.contains('\\') {
                bail!("unsafe transcript path segment {:?} in {}", seg, rel_str);
            }
        }
        if segments.len() < 2 {
            bail!(
                "transcript {} lacks a project/session (got {} segment(s))",
                rel_str,
                segments.len()
            );
        }

        let project = segments[0].to_string();
        let session_id = segments[1].to_string();
        let sub_path = segments[2..].join("/");

        // Runtime ARC-02 guard: the resolved directory must stay under archive_dir().
        let archive_dir = self.archive_dir();
        let mut resolved = archive_dir.join(&project).join(&session_id);
        if !sub_path.is_empty() {
            resolved = resolved.join(&sub_path);
        }
        if !resolved.starts_with(&archive_dir) {
            bail!(
                "resolved archive path {} escapes archive_dir {}",
                resolved.display(),
                archive_dir.display()
            );
        }

        Ok((project, session_id, sub_path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(base: &str) -> Config {
        Config {
            base_dir: PathBuf::from(base),
            idle_timeout_secs: 900,
            embed: EmbedConfig::default(),
        }
    }

    #[test]
    fn load_errors_when_base_dir_is_root() {
        let err = Config::from_toml_str("base_dir = \"/\"\n").unwrap_err();
        assert!(
            format!("{err:#}").contains("ARC-02"),
            "expected ARC-02 error, got: {err:#}"
        );
    }

    #[test]
    fn load_errors_when_base_dir_missing() {
        assert!(Config::from_toml_str("idle_timeout_secs = 5\n").is_err());
    }

    #[test]
    fn idle_timeout_defaults_to_900() {
        let c = Config::from_toml_str("base_dir = \"/data/hindsight\"\n").unwrap();
        assert_eq!(c.idle_timeout_secs, 900);
    }

    #[test]
    fn embed_defaults_when_table_absent() {
        let c = Config::from_toml_str("base_dir = \"/data/hindsight\"\n").unwrap();
        assert_eq!(c.embed.ollama_url, "http://127.0.0.1:11434");
        assert_eq!(c.embed.model, "qwen3-embedding:8b");
        assert_eq!(c.embed.keep_alive, "5m");
    }

    #[test]
    fn embed_partial_table_keeps_other_defaults() {
        let c = Config::from_toml_str(
            "base_dir = \"/data/hindsight\"\n[embed]\nkeep_alive = \"10m\"\n",
        )
        .unwrap();
        assert_eq!(c.embed.keep_alive, "10m");
        assert_eq!(c.embed.model, "qwen3-embedding:8b");
    }

    #[test]
    fn archive_dir_is_base_slash_archive() {
        let c = cfg("/data/hindsight");
        assert_eq!(c.archive_dir(), PathBuf::from("/data/hindsight/archive"));
        assert_eq!(c.state_dir(), PathBuf::from("/data/hindsight/state"));
    }

    #[test]
    fn index_dir_is_base_slash_index() {
        let c = cfg("/data/hindsight");
        assert_eq!(c.index_dir(), PathBuf::from("/data/hindsight/index"));
        assert_eq!(
            c.db_path(),
            PathBuf::from("/data/hindsight/index/hindsight.db")
        );
    }

    #[test]
    fn archive_key_maps_top_level_transcript() {
        let c = cfg("/data/hindsight");
        let root = Path::new("/home/u/.claude");
        let src = Path::new("/home/u/.claude/projects/proj/sess.jsonl");
        let (p, s, sub) = c.archive_key(root, src).unwrap();
        assert_eq!((p.as_str(), s.as_str(), sub.as_str()), ("proj", "sess", ""));
    }

    #[test]
    fn archive_key_maps_nested_subagent_transcript() {
        let c = cfg("/data/hindsight");
        let root = Path::new("/home/u/.claude");
        let src =
            Path::new("/home/u/.claude/projects/proj/sess/subagents/agent-abc.jsonl");
        let (p, s, sub) = c.archive_key(root, src).unwrap();
        assert_eq!(
            (p.as_str(), s.as_str(), sub.as_str()),
            ("proj", "sess", "subagents/agent-abc")
        );
    }

    #[test]
    fn archive_key_rejects_dotdot_segment() {
        let c = cfg("/data/hindsight");
        let root = Path::new("/home/u/.claude");
        let src = Path::new("/home/u/.claude/projects/proj/../etc/sess.jsonl");
        assert!(c.archive_key(root, src).is_err());
    }

    #[test]
    fn archive_key_rejects_path_outside_projects() {
        let c = cfg("/data/hindsight");
        let root = Path::new("/home/u/.claude");
        let src = Path::new("/somewhere/else/sess.jsonl");
        assert!(c.archive_key(root, src).is_err());
    }
}
