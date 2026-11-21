//! Test-side environment mutation has flaked two suites already: `set_var`
//! writes process-global state that every other test thread in the same
//! binary reads (each account or helper client copies the environment into
//! the children it spawns), so whichever test happens to be running when the
//! write lands can fail, and the location moves with timing.
//!
//! This guard scans the workspace's Rust sources for the two calls and fails
//! with the file and line. Tests inject roots through explicit parameters or
//! hidden setters instead; only the app's own startup is allowed to shape its
//! environment. Comments and string literals are ignored, so prose and this
//! guard's own patterns cannot trip it.

use std::fs;
use std::path::{Path, PathBuf};

/// The app sets `GTK_THEME`/`WEBKIT_*` once, before any thread starts.
const ALLOWED: &[&str] = &["apps/muse/src/main.rs"];

/// Directory names never scanned: build output, vendored sources, and the
/// web bundle's dependencies cannot contain workspace Rust tests.
const SKIP_DIRS: &[&str] = &[
    "target",
    "vendor",
    "node_modules",
    "dist",
    "build",
    ".git",
    ".freebuff",
];

const FORBIDDEN: &[&str] = &["set_var(", "remove_var("];

fn workspace_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let manifest = dir.join("Cargo.toml");
        if fs::read_to_string(&manifest).is_ok_and(|text| text.contains("[workspace]")) {
            return dir;
        }
        assert!(
            dir.pop(),
            "no workspace Cargo.toml above {}",
            env!("CARGO_MANIFEST_DIR")
        );
    }
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            rust_files(&path, out);
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

/// The source with comments and literals blanked out, newlines kept so line
/// numbers still match. A small scanner rather than a dependency: it only
/// needs to know where code ends and prose, strings and chars begin, and it
/// must not mistake a lifetime (`&'a str`) for a char literal.
fn code_only(source: &str) -> String {
    enum State {
        Code,
        Line,
        Block(u32),
        Text,
        Raw(usize),
    }
    let mut state = State::Code;
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        match state {
            State::Code => match c {
                '/' if chars.peek() == Some(&'/') => {
                    chars.next();
                    out.push(' ');
                    state = State::Line;
                }
                '/' if chars.peek() == Some(&'*') => {
                    chars.next();
                    out.push(' ');
                    state = State::Block(1);
                }
                '"' => {
                    out.push(' ');
                    state = State::Text;
                }
                '\'' => {
                    // A lifetime keeps its quote; only a closed literal is text.
                    let mut look = chars.clone();
                    let closes = match look.next() {
                        Some('\\') => {
                            look.next();
                            look.next() == Some('\'')
                        }
                        Some(_) => look.next() == Some('\''),
                        None => false,
                    };
                    if closes {
                        chars = look;
                        out.push(' ');
                    } else {
                        out.push('\'');
                    }
                }
                'r' => {
                    // Raw string: `r"…"` or `r#"…"#`.
                    let mut hashes = 0;
                    while chars.peek() == Some(&'#') {
                        chars.next();
                        hashes += 1;
                    }
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        out.push(' ');
                        state = State::Raw(hashes);
                    } else {
                        out.push('r');
                        for _ in 0..hashes {
                            out.push('#');
                        }
                    }
                }
                _ => out.push(c),
            },
            State::Line => {
                if c == '\n' {
                    out.push('\n');
                    state = State::Code;
                }
            }
            State::Block(depth) => match c {
                '\n' => out.push('\n'),
                '/' if chars.peek() == Some(&'*') => {
                    chars.next();
                    state = State::Block(depth + 1);
                }
                '*' if chars.peek() == Some(&'/') => {
                    chars.next();
                    if depth > 1 {
                        state = State::Block(depth - 1);
                    } else {
                        state = State::Code;
                    }
                }
                _ => {}
            },
            State::Text => match c {
                '\\' => {
                    chars.next();
                }
                '"' => state = State::Code,
                // Unterminated literal: recover at the next line rather than
                // silently swallowing the rest of the file.
                '\n' => {
                    out.push('\n');
                    state = State::Code;
                }
                _ => {}
            },
            State::Raw(hashes) => {
                if c == '"' {
                    let mut seen = 0;
                    while seen < hashes && chars.peek() == Some(&'#') {
                        chars.next();
                        seen += 1;
                    }
                    if seen == hashes {
                        state = State::Code;
                    }
                } else if c == '\n' {
                    out.push('\n');
                }
            }
        }
    }
    out
}

#[test]
fn no_source_mutates_the_process_environment() {
    let root = workspace_root();
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    // A wrong root would make this test vacuously pass.
    assert!(
        files
            .iter()
            .any(|path| path.ends_with("apps/muse/src/main.rs")),
        "scanned {} Rust files under {} without finding the app's main.rs",
        files.len(),
        root.display()
    );

    let mut offenders = Vec::new();
    for path in &files {
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if ALLOWED.contains(&relative.as_str()) {
            continue;
        }
        let Ok(source) = fs::read_to_string(path) else {
            continue;
        };
        let code = code_only(&source);
        for (index, line) in code.lines().enumerate() {
            if FORBIDDEN.iter().any(|needle| line.contains(needle)) {
                offenders.push(format!("{relative}:{}", index + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "process environment mutated outside {}:\n  {}\nInject a root/path explicitly, or pin the value on the type under test, \
         instead of writing shared process state.",
        ALLOWED.join(", "),
        offenders.join("\n  ")
    );
}
