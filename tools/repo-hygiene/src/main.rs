//! repo-hygiene — refuse original game payloads, raw reverse-engineering dumps
//! and private paths in this public repository.
//!
//! Run before every push (CI runs it too):
//!
//! ```text
//! cargo run -p repo-hygiene            # all tracked + untracked, non-ignored files
//! cargo run -p repo-hygiene -- --staged # only what is staged for commit
//! ```
//!
//! A line can opt out of the *text* checks with the marker `hygiene:allow`
//! (used for documentation that must quote a pattern). Binary/magic/size
//! checks cannot be opted out of.
//!
//! Raster images and clips are accepted in one place only, `docs/images/`:
//! curated screenshots and short clips of **our runtime** for the README
//! (see `docs/LEGAL.md`). Anywhere else an image file is refused, so an
//! extracted texture cannot slip in as a `.png`; texture container formats
//! are refused everywhere.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(about = "Check the repository for original game data and RE dumps")]
struct Args {
    /// Only check files staged for commit.
    #[arg(long)]
    staged: bool,
    /// Treat warnings (decompiler fingerprints) as failures.
    #[arg(long)]
    strict: bool,
    /// Repository root (defaults to the git toplevel of the current directory).
    #[arg(long)]
    root: Option<PathBuf>,
}

/// Extensions that are original game payloads or RE databases. Never committed.
const FORBIDDEN_EXT: &[&str] = &[
    "u",
    "upk",
    "asamu",
    "umap",
    "udk",
    "tfc",
    "int",
    "ue3profile",
    "bik",
    "swf",
    "gfx",
    "dylib",
    "dll",
    "exe",
    "so",
    "icns",
    "wav",
    "ogg",
    "mp3",
    "mp4",
    "xma",
    "fsb",
    "bnk",
    "pak",
    "gpr",
    "rep",
    "gzf",
    "idb",
    "i64",
    "bndb",
    "til",
    "nam",
    "id0",
    "id1",
    "id2",
    "lzo",
    "decompressed",
];

/// Largest file we accept. Our own sources, docs and sanitized metadata are small.
const MAX_BYTES: u64 = 1024 * 1024;

/// The only folder that may hold raster images: showcase screenshots and
/// short clips of our runtime.
const SHOWCASE_DIR: &str = "docs/images/";

/// Largest showcase file (a short README clip needs more than [`MAX_BYTES`]).
const SHOWCASE_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Showcase formats: (extension, the bytes such a file starts with).
const SHOWCASE_FORMATS: &[(&str, &[u8])] = &[
    ("png", b"\x89PNG\r\n\x1a\n"),
    ("jpg", &[0xFF, 0xD8, 0xFF]),
    ("jpeg", &[0xFF, 0xD8, 0xFF]),
    ("gif", b"GIF8"),
    ("webp", b"RIFF"),
];

/// Image and texture formats that are refused outside [`SHOWCASE_DIR`]
/// (the first five) or everywhere (texture containers and video).
const IMAGE_EXT: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "tga", "dds", "ktx", "ktx2", "tif", "tiff", "psd",
    "exr", "hdr", "avif", "mov", "webm", "mkv", "avi",
];

/// What the image rules say about `rel` (`len` bytes, starting with `head`):
/// `None` = not an image, or an acceptable showcase file.
fn image_finding(rel: &str, len: u64, head: &[u8]) -> Option<String> {
    let ext = Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)?;
    if !IMAGE_EXT.contains(&ext.as_str()) {
        return None;
    }
    let Some((_, magic)) = SHOWCASE_FORMATS.iter().find(|(e, _)| *e == ext) else {
        return Some(format!(
            "image/texture/video format .{ext} is never committed"
        ));
    };
    // Directly inside the folder: no sub-folders to hide a texture tree in.
    let in_showcase = rel
        .strip_prefix(SHOWCASE_DIR)
        .is_some_and(|name| !name.is_empty() && !name.contains('/'));
    if !in_showcase {
        return Some(format!(
            "image .{ext} outside {SHOWCASE_DIR} (only showcase screenshots of our runtime are \
             committed, and only there)"
        ));
    }
    if !head.starts_with(magic) {
        return Some(format!("content is not a .{ext} image"));
    }
    if len > SHOWCASE_MAX_BYTES {
        return Some(format!(
            "showcase file is {len} bytes (limit {SHOWCASE_MAX_BYTES})"
        ));
    }
    None
}

/// The size limit that applies to `rel`.
fn size_limit(rel: &str) -> u64 {
    if rel.starts_with(SHOWCASE_DIR) {
        SHOWCASE_MAX_BYTES
    } else {
        MAX_BYTES
    }
}

/// (magic bytes at offset 0, description)
const FORBIDDEN_MAGIC: &[(&[u8], &str)] = &[
    (&[0xC1, 0x83, 0x2A, 0x9E], "Unreal Engine 3 package"),
    (&[0xCF, 0xFA, 0xED, 0xFE], "Mach-O 64-bit binary"),
    (&[0xCE, 0xFA, 0xED, 0xFE], "Mach-O 32-bit binary"),
    (&[0xCA, 0xFE, 0xBA, 0xBE], "Mach-O universal binary"),
    (b"MZ", "PE/DOS executable"),
    (b"\x7FELF", "ELF binary"),
    (b"FWS", "Flash SWF"),
    (b"CWS", "Flash SWF (compressed)"),
    (b"GFX", "Scaleform GFx movie"),
    (b"BIK", "Bink video"),
];

/// Text fragments that indicate private paths. Literal fragments, not regexes.
const PRIVATE_PATH_FRAGMENTS: &[&str] = &["/Users/", "C:\\Users\\", "/home/"];

/// Text fragments that indicate pasted decompiler output (warning by default).
const DECOMPILER_FRAGMENTS: &[&str] = &[
    "FUN_0",
    "FUN_1",
    "DAT_0",
    "DAT_1",
    "LAB_0",
    "undefined8 ",
    "undefined4 ",
    "Decompiled with",
    "WARNING: Subroutine does not return",
];

const ALLOW_MARKER: &str = "hygiene:allow";

#[derive(Debug)]
struct Finding {
    path: String,
    fatal: bool,
    msg: String,
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .context("running git")?;
    if !out.status.success() {
        bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn check_file(root: &Path, rel: &str, findings: &mut Vec<Finding>) -> Result<()> {
    let path = root.join(rel);
    let Ok(meta) = fs::metadata(&path) else {
        return Ok(()); // deleted in working tree
    };
    if !meta.is_file() {
        return Ok(());
    }
    let mut push = |fatal: bool, msg: String| {
        findings.push(Finding {
            path: rel.to_string(),
            fatal,
            msg,
        })
    };

    let ext = Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    if let Some(ext) = &ext
        && FORBIDDEN_EXT.contains(&ext.as_str())
    {
        push(true, format!("forbidden extension .{ext}"));
    }
    let limit = size_limit(rel);
    if meta.len() > limit {
        push(
            true,
            format!("file is {} bytes (limit {limit})", meta.len()),
        );
    }

    let mut head = [0u8; 8];
    let n = fs::File::open(&path)?.read(&mut head)?;
    if let Some(msg) = image_finding(rel, meta.len(), &head[..n]) {
        push(true, msg);
    }
    for (magic, what) in FORBIDDEN_MAGIC {
        if n >= magic.len() && head[..magic.len()] == **magic {
            push(true, format!("content looks like a {what}"));
        }
    }

    // Text checks only for files that decode as UTF-8 and are small enough.
    if meta.len() <= MAX_BYTES
        && let Ok(text) = fs::read_to_string(&path)
    {
        for (lineno, line) in text.lines().enumerate() {
            if line.contains(ALLOW_MARKER) {
                continue;
            }
            for frag in PRIVATE_PATH_FRAGMENTS {
                if line.contains(frag) {
                    push(
                        true,
                        format!(
                            "line {}: private absolute path fragment {frag:?}",
                            lineno + 1
                        ),
                    );
                }
            }
            for frag in DECOMPILER_FRAGMENTS {
                if line.contains(frag) {
                    push(
                        false,
                        format!("line {}: decompiler fingerprint {frag:?}", lineno + 1),
                    );
                }
            }
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    let root = match args.root {
        Some(r) => r,
        None => PathBuf::from(git(Path::new("."), &["rev-parse", "--show-toplevel"])?.trim()),
    };
    let list = if args.staged {
        git(
            &root,
            &["diff", "--cached", "--name-only", "--diff-filter=ACMR"],
        )?
    } else {
        git(&root, &["ls-files", "-co", "--exclude-standard"])?
    };

    let mut findings = Vec::new();
    let mut count = 0usize;
    for rel in list.lines().filter(|l| !l.is_empty()) {
        // This tool necessarily contains the patterns it searches for.
        if rel == "tools/repo-hygiene/src/main.rs" {
            continue;
        }
        count += 1;
        check_file(&root, rel, &mut findings)?;
    }

    let fatal = findings.iter().filter(|f| f.fatal).count();
    let warn = findings.len() - fatal;
    for f in &findings {
        let tag = if f.fatal { "FAIL" } else { "WARN" };
        eprintln!("{tag} {}: {}", f.path, f.msg);
    }
    println!("repo-hygiene: checked {count} files — {fatal} failure(s), {warn} warning(s)");
    if fatal > 0 || (args.strict && warn > 0) {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    const JPG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0];
    const GIF: &[u8] = b"GIF89a";

    #[test]
    fn showcase_images_are_accepted_only_in_their_folder() {
        assert_eq!(
            image_finding("docs/images/workshop.png", 500_000, PNG),
            None
        );
        assert_eq!(
            image_finding("docs/images/workshop.JPG", 500_000, JPG),
            None
        );
        assert_eq!(
            image_finding("docs/images/grapple.gif", 6_000_000, GIF),
            None
        );
        // Anywhere else an image is refused, however small.
        for rel in [
            "workshop.png",
            "docs/workshop.png",
            "docs/images/textures/T_Rock_D.png",
            "crates/asamu-assets/tests/fixtures/t.png",
            "docs/imagesx/a.png",
        ] {
            assert!(image_finding(rel, 10, PNG).is_some(), "{rel}");
        }
    }

    #[test]
    fn showcase_files_must_be_what_their_name_says() {
        assert!(image_finding("docs/images/a.png", 10, JPG).is_some());
        assert!(image_finding("docs/images/a.gif", 10, b"MZ\x90\x00").is_some());
        assert!(image_finding("docs/images/a.png", 10, b"").is_some());
        assert!(image_finding("docs/images/a.gif", SHOWCASE_MAX_BYTES + 1, GIF).is_some());
        assert_eq!(
            image_finding("docs/images/a.gif", SHOWCASE_MAX_BYTES, GIF),
            None
        );
    }

    #[test]
    fn texture_and_video_formats_are_refused_everywhere() {
        for rel in [
            "docs/images/T_Rock_D.dds",
            "docs/images/T_Rock_D.tga",
            "docs/images/clip.mov",
            "docs/images/clip.webm",
            "a/b.ktx2",
            "a/b.bmp",
        ] {
            assert!(image_finding(rel, 10, PNG).is_some(), "{rel}");
        }
    }

    #[test]
    fn other_files_are_not_images() {
        assert_eq!(image_finding("progress/progress.svg", 10, b"<svg"), None);
        assert_eq!(image_finding("README.md", 10, b"# ASAMU"), None);
        assert_eq!(image_finding("docs/images/README.md", 10, b"# x"), None);
        assert_eq!(image_finding("Makefile", 10, b"all:"), None);
    }

    #[test]
    fn only_the_showcase_folder_gets_the_larger_limit() {
        assert_eq!(size_limit("docs/images/grapple.gif"), SHOWCASE_MAX_BYTES);
        assert_eq!(size_limit("docs/PARITY.md"), MAX_BYTES);
        assert_eq!(size_limit("src/docs/images/a.gif"), MAX_BYTES);
    }
}
