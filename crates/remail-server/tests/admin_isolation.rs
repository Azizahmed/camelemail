//! Admin isolation (design §13), static half: only the sync module may touch
//! user sections or user data keys. The dynamic half (call every admin route,
//! look for markers) comes with the admin API in M3.

use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Source without comments (doc comments may name what they forbid).
fn code(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn only_sections_decrypts_user_data() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let sections = src.join("sections");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(files.len() > 10);
    for f in &files {
        if f.starts_with(&sections) {
            continue;
        }
        let c = code(f);
        for needle in ["decrypt_user_section", "encrypt_user_section", "SyncPath(", "user_sections"] {
            assert!(!c.contains(needle), "{} uses {needle}: only src/sections/ may", f.display());
        }
        // `user_key` is defined in crypto/envelope.rs; nobody else calls it.
        if !f.ends_with("crypto/envelope.rs") {
            assert!(!c.contains("user_key("), "{} asks for a user data key", f.display());
        }
    }
}

#[test]
fn admin_module_has_no_path_to_user_data() {
    let admin = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/admin");
    let mut files = Vec::new();
    rust_files(&admin, &mut files);
    assert!(!files.is_empty());
    for f in &files {
        let c = code(f);
        for needle in ["sections", "user_key", "OwnerKind::User", "crypto::open", "decrypt"] {
            assert!(!c.contains(needle), "{} references {needle}", f.display());
        }
    }
}
