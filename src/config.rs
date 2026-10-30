use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use crate::error::JanusError;

/// The vault root is the directory that contains this file, so it is not
/// repeated inside the config. M1 will decide how `--vault`, the environment,
/// and ancestor discovery interact; one source of truth until then.
const DEFAULT_CONFIG: &str = "index_dir = \".janus\"\n";

const JANUS_DIR: &str = ".janus";

/// Resolves the vault directory path.
/// Uses the explicit path if provided, otherwise the current directory.
/// The result is absolute, symlink-free, and guaranteed to be a directory.
pub fn resolve_vault(explicit: Option<PathBuf>) -> Result<PathBuf, JanusError> {
    let requested = match explicit {
        Some(path) => path,
        None => std::env::current_dir()?,
    };

    let resolved = requested
        .canonicalize()
        .map_err(|source| JanusError::VaultResolve {
            path: requested,
            source,
        })?;

    if !resolved.is_dir() {
        return Err(JanusError::VaultNotDirectory { path: resolved });
    }

    Ok(resolved)
}

/// Initializes a new janus vault at the root path.
///
/// Never overwrites an existing `janus.toml`: the file is created with
/// `create_new`, which fails atomically instead of truncating.
pub fn init_vault(root: &Path) -> Result<PathBuf, JanusError> {
    let config_path = root.join("janus.toml");
    let index_dir = root.join(JANUS_DIR);

    fs::create_dir_all(&index_dir).map_err(|source| JanusError::CreateDir {
        path: index_dir,
        source,
    })?;

    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&config_path)
    {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            return Err(JanusError::ConfigExists { path: config_path });
        }
        Err(source) => {
            return Err(JanusError::Write {
                path: config_path,
                source,
            });
        }
    };

    if let Err(source) = file.write_all(DEFAULT_CONFIG.as_bytes()) {
        // Leave no half-written config behind, then surface the real cause.
        // The file must be closed first: Windows cannot remove an open file.
        drop(file);
        let _ = fs::remove_file(&config_path);
        return Err(JanusError::Write {
            path: config_path,
            source,
        });
    }

    warn_if_not_ignored(root);

    Ok(config_path)
}

/// Prints a hint when the index directory would otherwise be committed.
/// This only reports; it never edits `.gitignore` (SPEC §8.6, rule 3).
fn warn_if_not_ignored(root: &Path) {
    let gitignore = root.join(".gitignore");

    if !gitignore.exists() {
        tracing::info!("no .gitignore found; consider adding one that ignores `.janus/`");
        return;
    }

    match fs::read_to_string(&gitignore) {
        Ok(contents) if gitignore_ignores_janus(&contents) => {}
        Ok(_) => tracing::warn!(
            path = %gitignore.display(),
            "index directory is not git-ignored; add this line: .janus/"
        ),
        Err(error) => tracing::warn!(
            path = %gitignore.display(),
            %error,
            "could not read .gitignore; make sure it ignores `.janus/`"
        ),
    }
}

/// True when some non-comment line ignores the `.janus` directory.
/// Accepts `.janus`, `.janus/`, `/.janus`, and `/.janus/`.
fn gitignore_ignores_janus(contents: &str) -> bool {
    contents.lines().any(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return false;
        }
        line.trim_start_matches('/').trim_end_matches('/') == JANUS_DIR
    })
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
    fn test_resolve_vault_rejects_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file = temp_dir.path().join("not-a-vault.md");
        fs::write(&file, "hello").unwrap();

        let result = resolve_vault(Some(file));
        assert!(matches!(result, Err(JanusError::VaultNotDirectory { .. })));
    }

    #[test]
    fn test_resolve_vault_missing_path_names_the_path() {
        let temp_dir = tempfile::tempdir().unwrap();
        let missing = temp_dir.path().join("does-not-exist");

        let err = resolve_vault(Some(missing.clone())).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains(&missing.display().to_string()),
            "{message}"
        );
    }

    #[test]
    fn test_init_vault_success() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();

        let config_path = init_vault(root).unwrap();

        assert!(config_path.exists());
        assert!(root.join(".janus").is_dir());

        let content = fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("index_dir = \".janus\""));
    }

    #[test]
    fn test_init_vault_twice_fails() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();

        init_vault(root).unwrap();
        let result = init_vault(root);
        assert!(matches!(result, Err(JanusError::ConfigExists { .. })));
    }

    #[test]
    fn test_init_vault_never_overwrites_config() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();

        init_vault(root).unwrap();
        let config_path = root.join("janus.toml");
        fs::write(&config_path, "sentinel = true\n").unwrap();

        let result = init_vault(root);

        assert!(matches!(result, Err(JanusError::ConfigExists { .. })));
        assert_eq!(
            fs::read_to_string(&config_path).unwrap(),
            "sentinel = true\n",
            "existing config must be byte-identical after a refused init"
        );
    }

    #[test]
    fn gitignore_recognizes_janus_variants() {
        for contents in [
            ".janus",
            ".janus/",
            "/.janus",
            "/.janus/",
            "target\n.janus\n",
        ] {
            assert!(
                gitignore_ignores_janus(contents),
                "should match: {contents:?}"
            );
        }
    }

    #[test]
    fn gitignore_ignores_noise() {
        for contents in [
            "",
            "# .janus is intentionally committed\n",
            ".janus-old\n",
            "subdir/.janus\n",
        ] {
            assert!(
                !gitignore_ignores_janus(contents),
                "should not match: {contents:?}"
            );
        }
    }
}
