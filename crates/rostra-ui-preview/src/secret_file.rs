use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result, bail};

/// Maximum accepted secret size, protecting against accidental large-file
/// input.
const MAX_SECRET_BYTES: u64 = 16 * 1024;

/// Read this project's expected development secret for the configured port.
pub fn read_dev_secret(path: &Path, port: u16) -> Result<String> {
    read_dev_secret_from(path, port, &std::env::current_dir()?)
}

/// Bind secret input to `<project>/dev/<port>/secret` before opening it.
fn read_dev_secret_from(path: &Path, port: u16, project_root: &Path) -> Result<String> {
    let expected = project_root
        .join("dev")
        .join(port.to_string())
        .join("secret");
    let provided = if path.is_absolute() {
        path.to_owned()
    } else {
        project_root.join(path)
    };
    if provided != expected {
        bail!("secret path must be exactly dev/{port}/secret for the configured Rostra origin");
    }
    for directory in [
        project_root.join("dev"),
        project_root.join("dev").join(port.to_string()),
    ] {
        if fs::symlink_metadata(directory)
            .context("checking development secret directory")?
            .file_type()
            .is_symlink()
        {
            bail!("development secret path must not contain symlinked components");
        }
    }
    if fs::symlink_metadata(path)
        .context("reading development secret path")?
        .file_type()
        .is_symlink()
    {
        bail!("development secret path must not be a symlink");
    }
    let actual = fs::canonicalize(path).context("resolving development secret path")?;
    let expected =
        fs::canonicalize(expected).context("expected development secret does not exist")?;
    if actual != expected {
        bail!("secret path must be dev/{port}/secret for the configured Rostra origin");
    }
    read_protected(&actual)
}

/// Read a secret from a regular owner-only file without logging its value.
fn read_protected(path: &Path) -> Result<String> {
    let path_metadata = fs::symlink_metadata(path)
        .with_context(|| format!("reading secret-file metadata for {}", path.display()))?;
    if !path_metadata.file_type().is_file() {
        bail!("secret file must be a regular file, not a symlink or special file");
    }
    let file = File::open(path).context("opening secret file")?;
    let metadata = file.metadata().context("reading opened secret metadata")?;
    if path_metadata.dev() != metadata.dev() || path_metadata.ino() != metadata.ino() {
        bail!("secret file changed while it was being opened");
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        bail!(
            "secret file permissions must deny all group and other access (mode 0600 or stricter)"
        );
    }
    if metadata.len() > MAX_SECRET_BYTES {
        bail!("secret file exceeds the 16 KiB safety limit");
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_SECRET_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("reading secret file")?;
    if bytes.len() as u64 > MAX_SECRET_BYTES {
        bail!("secret file exceeds the 16 KiB safety limit");
    }
    let secret = String::from_utf8(bytes).context("secret file is not valid UTF-8")?;
    let secret = secret.trim_end_matches(['\r', '\n']).to_owned();
    if secret.is_empty() {
        bail!("secret file is empty");
    }
    if secret.contains('\0') {
        bail!("secret file contains a NUL byte");
    }
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use tempfile::tempdir;

    use super::{read_dev_secret_from, read_protected};

    #[test]
    fn reads_owner_only_file_and_trims_line_ending() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("secret");
        fs::write(&path, "not-a-real-secret\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        assert_eq!(read_protected(&path).unwrap(), "not-a-real-secret");
    }

    #[test]
    fn rejects_broad_permissions_and_symlinks() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("secret");
        fs::write(&path, "not-a-real-secret").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(read_protected(&path).is_err());

        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let link = directory.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(read_protected(&link).is_err());
    }

    #[test]
    fn enforces_content_and_size_contract() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("secret");
        let write = |bytes: &[u8]| {
            fs::write(&path, bytes).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        };

        write(b"");
        assert!(read_protected(&path).is_err());
        write(b"has\0nul");
        assert!(read_protected(&path).is_err());
        write(&[0xff]);
        assert!(read_protected(&path).is_err());
        write(&vec![b'x'; super::MAX_SECRET_BYTES as usize]);
        assert_eq!(
            read_protected(&path).unwrap().len(),
            super::MAX_SECRET_BYTES as usize
        );
        write(&vec![b'x'; super::MAX_SECRET_BYTES as usize + 1]);
        assert!(read_protected(&path).is_err());
        assert!(read_protected(directory.path()).is_err());
    }

    #[test]
    fn binds_secret_to_project_port() {
        let project = tempdir().unwrap();
        let secret_dir = project.path().join("dev/2345");
        fs::create_dir_all(&secret_dir).unwrap();
        let path = secret_dir.join("secret");
        fs::write(&path, "not-a-real-secret").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        assert_eq!(
            read_dev_secret_from(&path, 2345, project.path()).unwrap(),
            "not-a-real-secret"
        );
        assert!(read_dev_secret_from(&path, 3456, project.path()).is_err());
        let alternate = project.path().join("alternate");
        fs::write(&alternate, "not-a-real-secret").unwrap();
        fs::set_permissions(&alternate, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_dev_secret_from(&alternate, 2345, project.path()).is_err());
    }

    #[test]
    fn rejects_symlinked_development_directory() {
        let project = tempdir().unwrap();
        let actual = project.path().join("actual/2345");
        fs::create_dir_all(&actual).unwrap();
        let secret = actual.join("secret");
        fs::write(&secret, "not-a-real-secret").unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(project.path().join("actual"), project.path().join("dev")).unwrap();

        assert!(
            read_dev_secret_from(
                &project.path().join("dev/2345/secret"),
                2345,
                project.path()
            )
            .is_err()
        );
    }
}
