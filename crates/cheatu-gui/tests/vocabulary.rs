//! The GUI speaks one vocabulary (see the GUI revamp spec): no old names for
//! its controls, and no "pid"/"process" outside the Advanced view.

use std::fs;
use std::path::Path;

/// Words that name things the GUI no longer has, or calls differently.
const BANNED: &[&str] = &[
    "First scan",
    "Next scan",
    "saved addresses",
    "Select process",
    "Request root access",
    "limited privileges",
    "New cheat",
    "Elevate & attach",
    "Cheat table cleared",
    // Unticking Advanced is the way back to the guided steps.
    "Guided…",
];

/// Words banned as whole words (the spec's vocabulary: cheats, not entries).
const BANNED_WORDS: &[&str] = &["entries", "entry"];

/// Literals that may say "pid" or "process", matched exactly: the Advanced
/// process list's headers.
const ALLOWED: &[&str] = &["Process", "PID"];

/// Advanced-only windows, where technical words stay (see the spec).
const ADVANCED_FILES: &[&str] = &["pointer_window.rs"];

/// String literals in `src`, outside `#[cfg(test)]` modules, with their line.
fn literals(src: &str) -> Vec<(usize, String)> {
    let code = match src.find("#[cfg(test)]\nmod tests") {
        Some(i) => &src[..i],
        None => src,
    };
    let mut out = Vec::new();
    let mut chars = code.char_indices().peekable();
    let mut line = 1;
    while let Some((_, c)) = chars.next() {
        match c {
            '\n' => line += 1,
            // Skip comments to the end of the line.
            '/' if chars.peek().map(|(_, n)| *n) == Some('/') => {
                for (_, n) in chars.by_ref() {
                    if n == '\n' {
                        line += 1;
                        break;
                    }
                }
            }
            // Character literals such as '"'.
            '\'' => {
                let rest: String = chars.clone().take(3).map(|(_, c)| c).collect();
                if rest.starts_with("\"'") || rest.starts_with("\\\"") {
                    chars.next();
                    chars.next();
                }
            }
            '"' => {
                let start = line;
                let mut s = String::new();
                while let Some((_, n)) = chars.next() {
                    match n {
                        '\\' => {
                            if let Some((_, e)) = chars.next() {
                                if e == '\n' {
                                    line += 1;
                                }
                                s.push(e);
                            }
                        }
                        '"' => break,
                        '\n' => {
                            line += 1;
                            s.push(' ');
                        }
                        _ => s.push(n),
                    }
                }
                out.push((start, s));
            }
            _ => {}
        }
    }
    out
}

/// `{placeholder}`s removed: they are filled in, not shown.
fn without_placeholders(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '{' => depth += 1,
            '}' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// The lower-case words of `s`, placeholders removed.
fn words(s: &str) -> Vec<String> {
    without_placeholders(s)
        .split(|c: char| !c.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn mentions_pid_or_process(s: &str) -> bool {
    words(s)
        .iter()
        .any(|w| w == "pid" || w == "process" || w == "processes")
}

#[test]
fn gui_text_uses_one_vocabulary() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut problems = Vec::new();
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let advanced_only = ADVANCED_FILES.contains(&name.as_str());
        let src = fs::read_to_string(&path).unwrap();
        for (line, lit) in literals(&src) {
            for b in BANNED {
                if lit.contains(b) {
                    problems.push(format!("{name}:{line}: \"{b}\" in {lit:?}"));
                }
            }
            for w in BANNED_WORDS {
                if words(&lit).iter().any(|x| x == w) {
                    problems.push(format!("{name}:{line}: \"{w}\" in {lit:?}"));
                }
            }
            if !advanced_only && mentions_pid_or_process(&lit) && !ALLOWED.contains(&lit.as_str()) {
                problems.push(format!("{name}:{line}: pid/process in {lit:?}"));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "{} problems:\n{}",
        problems.len(),
        problems.join("\n")
    );
}
