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
    if meta.len() > MAX_BYTES {
        push(
            true,
            format!("file is {} bytes (limit {MAX_BYTES})", meta.len()),
        );
    }

    let mut head = [0u8; 8];
    let n = fs::File::open(&path)?.read(&mut head)?;
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
