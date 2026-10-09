//! Bounded file reading and validation of paths stored in manifests.

use std::io::Read;
use std::path::Path;

use crate::error::{AssetError, AssetResult};

/// Largest manifest accepted (`textures/`, `meshes/`, `materials/`).
pub const MAX_MANIFEST_BYTES: u64 = 256 * 1024 * 1024;
/// Largest scene JSON accepted. The biggest shipped map converts to a few
/// tens of megabytes; this leaves generous headroom.
pub const MAX_SCENE_BYTES: u64 = 1024 * 1024 * 1024;

/// Reads a whole file, refusing files larger than `limit` bytes (checked
/// before and while reading, so a growing file cannot exceed it either).
///
/// # Errors
/// I/O failures and files over the limit.
pub fn read_bounded(path: &Path, limit: u64) -> AssetResult<Vec<u8>> {
    let io = |source| AssetError::Io {
        path: path.to_path_buf(),
        source,
    };
    let file = std::fs::File::open(path).map_err(io)?;
    let size = file.metadata().map_err(io)?.len();
    if size > limit {
        return Err(AssetError::TooLarge {
            path: path.to_path_buf(),
            size,
            limit,
        });
    }
    let mut data = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    file.take(limit.saturating_add(1))
        .read_to_end(&mut data)
        .map_err(io)?;
    let read = u64::try_from(data.len()).unwrap_or(u64::MAX);
    if read > limit {
        return Err(AssetError::TooLarge {
            path: path.to_path_buf(),
            size: read,
            limit,
        });
    }
    Ok(data)
}

/// Parses `data` (read from `path`) as JSON of type `T`.
///
/// # Errors
/// Malformed JSON or a document of the wrong shape.
pub fn parse_json<T: serde::de::DeserializeOwned>(path: &Path, data: &[u8]) -> AssetResult<T> {
    serde_json::from_slice(data).map_err(|source| AssetError::Json {
        path: path.to_path_buf(),
        source,
    })
}

/// Validates a relative path stored in a manifest and returns it normalized
/// with `/` separators, suitable both for joining onto a directory and for a
/// Bevy asset path.
///
/// Refused: empty paths, absolute paths (leading `/` or `\`, drive or URL
/// prefixes with `:`), `.` and `..` components, empty components, backslashes
/// (the importer always writes `/`), NUL and other control characters, and
/// `#` (Bevy's sub-asset label separator).
///
/// # Errors
/// [`AssetError::UnsafePath`] with the reason.
pub fn safe_relative_path(path: &str) -> AssetResult<String> {
    let refuse = |reason| AssetError::UnsafePath {
        path: path.to_owned(),
        reason,
    };
    if path.is_empty() {
        return Err(refuse("empty"));
    }
    if path.len() > 4096 {
        return Err(refuse("longer than 4096 bytes"));
    }
    if path.starts_with('/') {
        return Err(refuse("absolute"));
    }
    if path.contains('\\') {
        return Err(refuse("contains a backslash"));
    }
    if path.contains(':') {
        return Err(refuse("contains ':' (drive letter or URL scheme)"));
    }
    if path.contains('#') {
        return Err(refuse("contains '#'"));
    }
    if path.chars().any(char::is_control) {
        return Err(refuse("contains a control character"));
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" => return Err(refuse("empty component")),
            "." | ".." => return Err(refuse("'.' or '..' component")),
            p => parts.push(p),
        }
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_relative_paths() {
        assert_eq!(
            safe_relative_path("AG-Workshop/Pkg/Mesh.gltf").unwrap(),
            "AG-Workshop/Pkg/Mesh.gltf"
        );
        assert_eq!(safe_relative_path("a.dds").unwrap(), "a.dds");
        assert_eq!(
            safe_relative_path("A B/name@Pkg.gltf").unwrap(),
            "A B/name@Pkg.gltf"
        );
    }

    #[test]
    fn refuses_escapes_and_odd_paths() {
        for bad in [
            "",
            "/etc/passwd",
            "../x.dds",
            "a/../../b",
            "a/./b",
            "a//b",
            "a/",
            "C:/x",
            "c:x",
            "file://x",
            "a\\b",
            "\\\\server\\x",
            "a\u{0}b",
            "a\nb",
            "mesh.gltf#Mesh0",
        ] {
            assert!(safe_relative_path(bad).is_err(), "{bad:?} accepted");
        }
        assert!(safe_relative_path(&"a/".repeat(3000)).is_err());
    }

    #[test]
    fn bounded_read_enforces_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.json");
        std::fs::write(&p, b"0123456789").unwrap();
        assert_eq!(read_bounded(&p, 10).unwrap().len(), 10);
        assert!(matches!(
            read_bounded(&p, 9),
            Err(AssetError::TooLarge { size: 10, .. })
        ));
        assert!(matches!(
            read_bounded(&dir.path().join("missing"), 9),
            Err(AssetError::Io { .. })
        ));
    }

    #[test]
    fn json_errors_name_the_file() {
        let err = parse_json::<Vec<u32>>(Path::new("x.json"), b"[1, oops]").unwrap_err();
        assert!(err.to_string().contains("x.json"), "{err}");
    }
}
