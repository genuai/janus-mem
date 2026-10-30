use std::fs;
use std::path::{Path, PathBuf};

use crate::error::JanusError;

const DEFAULT_CONFIG: &str = r#"vault = "."
index_dir = ".janus"
"#;

/// Resolves the vault directory path.
/// Uses explicit path if provided, otherwise defaults to the current directory
pub fn resolve_vault(explicit: Option<PathBuf>) -> Result<PathBuf, JanusError> {
    match explicit {
        Some(path) => Ok(path.canonicalize()?),
        None => Ok(std::env::current_dir()?.canonicalize()?),
    }
}

/// Initializes a new janus vault at the root path
pub fn init_vault(root: &Path) -> Result<PathBuf, JanusError> {
    const JANUS_DIR: &str = ".janus";
    let config_path = root.join("janus.toml");
    if config_path.exists() {
        return Err(JanusError::ConfigExists { path: config_path });
    }

    let index_dir = root.join(JANUS_DIR);
    fs::create_dir_all(&index_dir)?;

    fs::write(&config_path, DEFAULT_CONFIG).map_err(|source| JanusError::Write {
        path: config_path.clone(),
        source,
    })?;

    let gitignore_path = root.join(".gitignore");
    if gitignore_path.exists()
        && fs::read_to_string(&gitignore_path).is_ok_and(|c| !c.contains(JANUS_DIR))
    {
        eprintln!("Warning: Add .janus/ to your .gitignore file.");
    }

    Ok(config_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_resolve_vault_explicit() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().to_path_buf();

        let resolved = resolve_vault(Some(path.clone())).unwrap();
        assert_eq!(resolved, path.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_vault_default() {
        let resolved = resolve_vault(None).unwrap();
        let expected = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert_eq!(resolved, expected);
    }

    #[test]
    fn test_init_vault_success() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();

        let config_path = init_vault(root).unwrap();

        assert!(config_path.exists());
        assert!(root.join(".janus").is_dir());

        let content = fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("vault = \".\""));
        assert!(content.contains("index_dir = \".janus\""));
    }

    #[test]
    fn test_init_vault_already_exists() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();

        // first init should succeed
        init_vault(root).unwrap();

        // second init should fail w/ ConfigExists
        let result = init_vault(root);
        assert!(matches!(result, Err(JanusError::ConfigExists { .. })))
    }
}
