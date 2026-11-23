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
//!
//! The last test pins the guard's own enforcement: `.github/workflows/rust.yml`
//! must run it from a step of its own, so narrowing the workspace test
//! invocation cannot quietly stop running it.

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

/// The workflow that must keep running the guard from its own step, and the
/// step's name and command.
const WORKFLOW: &str = ".github/workflows/rust.yml";

/// The bridge and the window on the other end of it, both relative to the
/// workspace root; the socket guard reads them and nothing else.
const BRIDGE: &str = "apps/muse/src/web_shell.rs";
const WINDOW: &str = "apps/muse-window/src/main.rs";
const GUARD_STEP: &str = "Run environment mutation guard";
const GUARD_COMMAND: &str = "cargo test -p muse --locked --test env_guard";

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

fn is_allowed(relative: &str) -> bool {
    ALLOWED.contains(&relative)
}

/// The 1-based lines of `source` that call a forbidden function, with
/// comments and literals ignored.
fn forbidden_lines(source: &str) -> Vec<usize> {
    code_only(source)
        .lines()
        .enumerate()
        .filter(|(_, line)| FORBIDDEN.iter().any(|needle| line.contains(needle)))
        .map(|(index, _)| index + 1)
        .collect()
}

/// `path:line` for every offending call in one file, empty when the file is
/// the allowed startup file.
fn offenders(relative: &str, source: &str) -> Vec<String> {
    if is_allowed(relative) {
        return Vec::new();
    }
    forbidden_lines(source)
        .into_iter()
        .map(|line| format!("{relative}:{line}"))
        .collect()
}

/// The workflow's `steps:` entries, one text block per step, so a command can
/// be located in the step that owns it instead of anywhere in the file. Each
/// line loses its indentation and any leading `- ` item marker.
fn steps(workflow: &str) -> Vec<String> {
    let mut steps: Vec<String> = Vec::new();
    let mut indent = 0;
    let mut in_steps = false;
    for line in workflow.lines() {
        let trimmed = line.trim();
        let leading = line.len() - line.trim_start().len();
        if !in_steps {
            if trimmed == "steps:" {
                in_steps = true;
                indent = leading;
            }
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        // `steps:` may share its indentation with its own `- ` items.
        if leading < indent {
            break;
        }
        if trimmed.starts_with("- ") {
            steps.push(String::new());
        }
        if let Some(step) = steps.last_mut() {
            step.push_str(trimmed.strip_prefix("- ").unwrap_or(trimmed));
            step.push('\n');
        }
    }
    steps
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

    let mut found = Vec::new();
    for path in &files {
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let Ok(source) = fs::read_to_string(path) else {
            continue;
        };
        found.extend(offenders(&relative, &source));
    }
    assert!(
        found.is_empty(),
        "process environment mutated outside {}:\n  {}\nInject a root/path explicitly, or pin the value on the type under test, \
         instead of writing shared process state.",
        ALLOWED.join(", "),
        found.join("\n  ")
    );
}

#[test]
fn ci_runs_the_guard_as_its_own_step() {
    let root = workspace_root();
    let path = root.join(WORKFLOW);
    let workflow =
        fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let steps = steps(&workflow);
    // A workflow shape this parser cannot follow must fail loudly, not pass.
    assert!(!steps.is_empty(), "no steps found in {}", path.display());

    let named: Vec<&String> = steps
        .iter()
        .filter(|step| {
            step.lines()
                .any(|line| line.strip_prefix("name:").map(str::trim) == Some(GUARD_STEP))
        })
        .collect();
    assert_eq!(
        named.len(),
        1,
        "{WORKFLOW} needs exactly one `{GUARD_STEP}` step, found {}",
        named.len()
    );

    let step = named[0];
    assert!(
        step.lines().any(|line| line.starts_with("run:"))
            && step.lines().any(|line| line.contains(GUARD_COMMAND)),
        "`{GUARD_STEP}` must run `{GUARD_COMMAND}` itself, got:\n{step}"
    );
}

/// The app's bridge to the shell window is the parent's pipes, and it must
/// stay that way. A socket is reachable by any local process, so the page's
/// command surface -- sends, decisions, grants -- would be anyone's to call; a
/// deleted spike once opened exactly that. The pipe is asserted here, not left
/// to a live run.
#[test]
fn the_shell_window_bridge_is_a_pipe_not_a_socket() {
    let root = workspace_root();
    let bridge = root.join(BRIDGE);
    let window = root.join(WINDOW);
    let pipe = code_only(
        &fs::read_to_string(&bridge)
            .unwrap_or_else(|error| panic!("{}: {error}", bridge.display())),
    );
    let window_code = code_only(
        &fs::read_to_string(&window)
            .unwrap_or_else(|error| panic!("{}: {error}", window.display())),
    );

    assert!(
        pipe.contains("Stdio::piped("),
        "{} must spawn the window on a pipe",
        bridge.display()
    );
    assert!(
        !root.join("apps/muse/src/spike_cmd.rs").exists(),
        "the spike's socket module is deleted, not dormant"
    );
    for (path, source) in [(BRIDGE, &pipe), (WINDOW, &window_code)] {
        for needle in ["UnixListener", "UnixStream", "MUSE_SPIKE_CMD"] {
            assert!(!source.contains(needle), "{path} still names {needle}");
        }
    }
    for manifest in ["apps/muse/Cargo.toml", "apps/muse-window/Cargo.toml"] {
        let path = root.join(manifest);
        let text = fs::read_to_string(&path).unwrap_or_else(|error| panic!("{manifest}: {error}"));
        assert!(
            !text.contains("spike-cmd") && !text.contains("spike_cmd"),
            "{manifest} still declares the spike's socket feature"
        );
    }
}

/// The scanner decides what the whole guard reports, so it is exercised on
/// synthetic sources rather than trusted: a regression here (a lifetime read
/// as a char literal, say) would let the guard pass silently.
#[cfg(test)]
mod tests {
    use super::*;

    fn lines(source: &str) -> Vec<usize> {
        forbidden_lines(source)
    }

    #[test]
    fn a_call_in_code_is_reported_with_its_line() {
        let source = "fn main() {\n    std::env::set_var(\"A\", \"B\");\n    std::env::remove_var(\"C\");\n}\n";
        assert_eq!(lines(source), vec![2, 3]);
    }

    #[test]
    fn calls_named_only_in_comments_are_ignored() {
        let source = "\
// std::env::set_var(\"A\", \"B\");\n\
/* std::env::remove_var(\"C\"); */\n\
/* outer std::env::set_var(\"D\", \"E\"); /* nested remove_var(\"F\") */ still prose */\n\
fn main() {}\n";
        assert_eq!(lines(source), Vec::<usize>::new());
    }

    #[test]
    fn a_line_comment_ends_at_the_newline() {
        let source = "// set_var( in prose\nstd::env::remove_var(\"X\");\n";
        assert_eq!(lines(source), vec![2]);
    }

    #[test]
    fn a_call_after_a_block_comment_is_still_seen() {
        let source = "/* set_var( prose) */ std::env::set_var(\"A\", \"B\");\n";
        assert_eq!(lines(source), vec![1]);
    }

    #[test]
    fn calls_named_only_in_strings_are_ignored() {
        let source = "\
fn main() {\n\
    let a = \"std::env::set_var(\\\"A\\\", \\\"B\\\")\";\n\
    let b = \"set_var( and remove_var( in one string\";\n\
    let c = '\"';\n\
    let d = '\\'';\n\
}\n";
        assert_eq!(lines(source), Vec::<usize>::new());
    }

    #[test]
    fn a_call_after_a_string_on_the_same_line_is_still_seen() {
        let source = "let s = \"set_var(\"; std::env::remove_var(\"X\");\n";
        assert_eq!(lines(source), vec![1]);
    }

    #[test]
    fn calls_named_only_in_raw_strings_are_ignored() {
        let source = "\
fn main() {\n\
    let a = r\"std::env::set_var(\\\"A\\\")\";\n\
    let b = r#\"std::env::remove_var(\\\"C\\\") \\\"quoted #\\\" text\"#;\n\
    let c = br#\"set_var(\"#;\n\
}\n";
        assert_eq!(lines(source), Vec::<usize>::new());
    }

    #[test]
    fn a_call_after_a_raw_string_is_still_seen() {
        let source = "let s = r#\"set_var( prose\"#; std::env::set_var(\"A\", \"B\");\n";
        assert_eq!(lines(source), vec![1]);
    }

    #[test]
    fn a_lifetime_never_swallows_the_code_after_it() {
        // The regression this forbids: reading `'a` as a char literal treats
        // what follows as literal text and reports nothing. The call sits on
        // the same line as the lifetime, so the scanner's end-of-line recovery
        // cannot mask the mistake.
        let source = "\
fn first<'a>(text: &'a str) -> &'a str {\n\
    std::env::set_var(\"A\", \"B\");\n\
    text\n\
}\n\
fn same_line<'a>(text: &'a str) { std::env::remove_var(\"C\"); }\n";
        assert_eq!(lines(source), vec![2, 5]);
    }

    #[test]
    fn a_call_after_a_char_literal_is_still_seen() {
        let source = "let quote = '\"'; std::env::remove_var(\"X\");\n";
        assert_eq!(lines(source), vec![1]);
    }

    #[test]
    fn workflow_steps_are_split_at_their_own_markers() {
        let workflow = "\
jobs:\n\
  build:\n\
    runs-on: ubuntu-latest\n\
    steps:\n\
    - name: Run tests\n\
      run: cargo test --workspace\n\
    - name: Lint\n\
      run: |\n\
        cargo clippy\n\
    - uses: actions/checkout@v4\n";
        let steps = steps(workflow);
        assert_eq!(steps.len(), 3);
        assert!(steps[0].contains("run: cargo test --workspace"));
        assert!(
            !steps[0].contains("cargo clippy"),
            "steps leaked: {steps:?}"
        );
        assert!(steps[1].contains("cargo clippy"));
        assert!(steps[2].contains("actions/checkout"));
    }

    #[test]
    fn a_workflow_without_a_steps_list_yields_nothing() {
        assert!(steps("jobs:\n  build:\n    runs-on: ubuntu-latest\n").is_empty());
    }

    #[test]
    fn only_the_app_startup_file_is_allowed() {
        let call = "std::env::set_var(\"GTK_THEME\", \"Adwaita\");\n";
        assert!(offenders("apps/muse/src/main.rs", call).is_empty());
        for path in [
            "apps/muse/src/session.rs",
            "apps/muse/src/main.rs.bak",
            "crates/muse-local-tools/tests/mutation.rs",
        ] {
            assert_eq!(offenders(path, call), vec![format!("{path}:1")]);
        }
    }
}
