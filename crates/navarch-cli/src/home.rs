//! Resolve the state directory during CLI startup, keeping storage free of
//! environment and user-home discovery concerns.

use std::{env, ffi::OsString, fs, path::PathBuf};

use anyhow::{Context, Result, bail};

/// Resolve `NAVARCH_HOME` or `~/.navarch` and create the directory.
///
/// On Unix, the directory is created with mode 0700. Existing directories have
/// their permissions restricted to the same mode. An empty override is an error.
///
/// # Errors
///
/// Returns an error if no home can be resolved or the directory cannot be created
/// or secured.
pub fn home() -> Result<PathBuf> {
    resolve_and_create(env::var_os("NAVARCH_HOME"), env::home_dir())
}

fn resolve_and_create(
    override_home: Option<OsString>,
    user_home: Option<PathBuf>,
) -> Result<PathBuf> {
    let path = match override_home {
        Some(value) if value.is_empty() => bail!("NAVARCH_HOME must not be empty"),
        Some(value) => PathBuf::from(value),
        None => user_home
            .context("cannot resolve the user home directory; set NAVARCH_HOME")?
            .join(".navarch"),
    };

    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&path)
        .with_context(|| format!("cannot create Navarch home at {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("cannot secure Navarch home at {}", path.display()))?;
    }
    Ok(path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn override_takes_precedence_and_creates_parents() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("nested/state");
        let result = resolve_and_create(Some(path.clone().into_os_string()), None).unwrap();
        assert_eq!(result, path);
        assert!(path.is_dir());
    }

    #[test]
    fn falls_back_to_dot_navarch() {
        let temp = tempfile::tempdir().unwrap();
        let path = resolve_and_create(None, Some(temp.path().to_path_buf())).unwrap();
        assert_eq!(path, temp.path().join(".navarch"));
        assert!(path.is_dir());
    }

    #[test]
    fn missing_home_and_empty_override_fail() {
        assert!(resolve_and_create(None, None).is_err());
        assert!(resolve_and_create(Some(OsString::new()), None).is_err());
    }

    #[test]
    fn file_instead_of_directory_fails() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("file");
        fs::write(&path, "not a directory").unwrap();
        assert!(resolve_and_create(Some(path.into_os_string()), None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn creates_private_directory_and_secures_existing_directory() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state");
        let override_home = Some(path.clone().into_os_string());
        resolve_and_create(override_home.clone(), None).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );

        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        resolve_and_create(override_home, None).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
