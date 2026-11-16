//! Tasks of a project (build, test, run…): found in its files or written
//! by hand in `.simpleedit/tasks.json`, run with their output streamed, and
//! the places an output names (`src/a.rs:12:5`) found in it.

use std::path::{Path, PathBuf};

/// A command of a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSpec {
    pub name: String,
    pub command: String,
    pub dir: PathBuf,
    /// Written in a project's own `tasks.json`: the project, which must be
    /// trusted for it to run (not its `cwd`, which the file chooses).
    pub custom: Option<PathBuf>,
}

/// A place an output line names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    /// From 1.
    pub line: usize,
    /// From 1; 1 when the line gives none.
    pub col: usize,
}

/// What a run says as it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Line(String),
    /// Lines that came together (the stream sends them in packets).
    Lines(Vec<String>),
    /// The exit code (`None`: stopped by a signal, or never started).
    Done(Option<i32>),
}

fn spec(name: &str, command: &str, dir: &Path) -> TaskSpec {
    TaskSpec {
        name: name.to_string(),
        command: command.to_string(),
        dir: dir.to_path_buf(),
        custom: None,
    }
}

/// The targets of a Makefile that are not patterns, variables or hidden.
fn make_targets(text: &str) -> Vec<String> {
    let mut targets: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.starts_with([' ', '\t', '#', '.']) {
            continue;
        }
        let Some((head, tail)) = line.split_once(':') else {
            continue;
        };
        // `a := 1` and `a::` are not targets of their own.
        if tail.starts_with('=') || head.contains(['=', '%', '$']) {
            continue;
        }
        for name in head.split_whitespace() {
            if !targets.iter().any(|t| t == name) {
                targets.push(name.to_string());
            }
        }
    }
    targets
}

/// The tasks of the project at `root`: the ones written in
/// `.simpleedit/tasks.json` (`[{"name", "command", "cwd"}]`) first, then
/// those its files imply (Cargo, npm scripts, Make, Go).
pub fn detect(root: &Path) -> Vec<TaskSpec> {
    let mut tasks = Vec::new();
    if let Ok(raw) = std::fs::read_to_string(root.join(".simpleedit").join("tasks.json")) {
        if let Ok(list) = serde_json::from_str::<Vec<serde_json::Value>>(&raw) {
            for item in list {
                let (Some(name), Some(command)) = (item["name"].as_str(), item["command"].as_str())
                else {
                    continue;
                };
                let dir = item["cwd"]
                    .as_str()
                    .map_or_else(|| root.to_path_buf(), |c| root.join(c));
                tasks.push(TaskSpec {
                    custom: Some(root.to_path_buf()),
                    ..spec(name, command, &dir)
                });
            }
        }
    }
    if root.join("Cargo.toml").is_file() {
        for (name, command) in [
            ("cargo build", "cargo build"),
            ("cargo run", "cargo run"),
            ("cargo test", "cargo test"),
            ("cargo check", "cargo check"),
            ("cargo clippy", "cargo clippy"),
            ("cargo fmt", "cargo fmt"),
        ] {
            tasks.push(spec(name, command, root));
        }
    }
    if let Ok(raw) = std::fs::read_to_string(root.join("package.json")) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(scripts) = json["scripts"].as_object() {
                for name in scripts.keys() {
                    tasks.push(spec(
                        &format!("npm run {name}"),
                        &format!("npm run {}", quote(name)),
                        root,
                    ));
                }
            }
        }
    }
    for name in ["Makefile", "makefile", "GNUmakefile"] {
        if let Ok(text) = std::fs::read_to_string(root.join(name)) {
            for target in make_targets(&text) {
                tasks.push(spec(
                    &format!("make {target}"),
                    &format!("make -- {}", quote(&target)),
                    root,
                ));
            }
            break;
        }
    }
    if root.join("go.mod").is_file() {
        tasks.push(spec("go build", "go build ./...", root));
        tasks.push(spec("go test", "go test ./...", root));
        tasks.push(spec("go vet", "go vet ./...", root));
    }
    tasks
}

/// The first place a line of output names that is a file: `path:12:5`,
/// `path:12`, `path(12,5)` (TypeScript), `--> path:12:5` (Rust) or
/// `File "path", line 12` (Python). Relative paths are from `dir`.
#[cfg(test)]
pub fn location(line: &str, dir: &Path) -> Option<Location> {
    location_with(line, dir, &mut |p| p.is_file())
}

/// [`location`], asking `is_file` whether a path is a file (so that a caller
/// reading many lines can remember the answers).
pub fn location_with(
    line: &str,
    dir: &Path,
    is_file: &mut dyn FnMut(&Path) -> bool,
) -> Option<Location> {
    use std::sync::OnceLock;
    static PYTHON: OnceLock<regex::Regex> = OnceLock::new();
    static PLAIN: OnceLock<regex::Regex> = OnceLock::new();
    let python = PYTHON.get_or_init(|| {
        regex::Regex::new(r#"File "([^"]+)", line (\d+)"#).expect("a valid pattern")
    });
    let plain = PLAIN.get_or_init(|| {
        regex::Regex::new(r#"([^\s:()"'<>|]+\.[A-Za-z0-9_]+)(?::|\()(\d+)(?:[:,](\d+))?"#)
            .expect("a valid pattern")
    });
    static SPACED: OnceLock<regex::Regex> = OnceLock::new();
    let spaced = SPACED.get_or_init(|| {
        regex::Regex::new(r#"(/[^:()"'<>|]+\.[A-Za-z0-9_]+)(?::|\()(\d+)(?:[:,](\d+))?"#)
            .expect("a valid pattern")
    });
    let mut resolve = |path: &str| {
        let path = PathBuf::from(path);
        let full = if path.is_absolute() {
            path
        } else {
            dir.join(path)
        };
        is_file(&full).then_some(full)
    };
    if let Some(c) = python.captures(line) {
        if let Some(path) = resolve(&c[1]) {
            return Some(Location {
                path,
                line: c[2].parse().ok()?,
                col: 1,
            });
        }
    }
    for c in plain.captures_iter(line) {
        let Some(path) = resolve(&c[1]) else { continue };
        return Some(Location {
            path,
            line: c[2].parse().ok()?,
            col: c.get(3).and_then(|m| m.as_str().parse().ok()).unwrap_or(1),
        });
    }
    // An absolute path with spaces: the longest tail that is a file.
    for c in spaced.captures_iter(line) {
        let whole = &c[1];
        let starts = whole.match_indices('/').map(|(i, _)| i);
        for start in starts {
            if let Some(path) = resolve(&whole[start..]) {
                return Some(Location {
                    path,
                    line: c[2].parse().ok()?,
                    col: c.get(3).and_then(|m| m.as_str().parse().ok()).unwrap_or(1),
                });
            }
        }
    }
    None
}

/// A line without the escape sequences that color or move the cursor.
pub fn strip_ansi(line: &str) -> String {
    use std::sync::OnceLock;
    static ESCAPES: OnceLock<regex::Regex> = OnceLock::new();
    let escapes = ESCAPES.get_or_init(|| {
        regex::Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\r").expect("a valid pattern")
    });
    escapes.replace_all(line, "").into_owned()
}

/// A word for `sh`, in single quotes.
pub fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tasks_of_a_project_come_from_its_files_and_its_own_list() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("Cargo.toml"), "").unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"scripts": {"build": "x", "lint": "y"}}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("Makefile"),
            "CC := gcc\n.PHONY: all\nall: a.o\n\tcc\nclean:\n\trm\n%.o: %.c\n\tcc\ninstall test: all\n",
        )
        .unwrap();
        std::fs::create_dir(root.join(".simpleedit")).unwrap();
        std::fs::write(
            root.join(".simpleedit/tasks.json"),
            r#"[{"name": "serve", "command": "python -m http.server", "cwd": "web"}, {"name": "no command"}]"#,
        )
        .unwrap();
        let names: Vec<String> = detect(root).into_iter().map(|t| t.name).collect();
        assert_eq!(names[0], "serve");
        for wanted in [
            "cargo test",
            "npm run build",
            "npm run lint",
            "make all",
            "make clean",
            "make install",
            "make test",
        ] {
            assert!(names.contains(&wanted.to_string()), "{wanted}: {names:?}");
        }
        assert!(!names.iter().any(|n| n.contains('%') || n.contains("CC")));
        assert_eq!(detect(root)[0].dir, root.join("web"));
        // What must be trusted is the project, whatever folder it runs in.
        assert_eq!(detect(root)[0].custom.as_deref(), Some(root));
        assert!(detect(root).iter().skip(1).all(|t| t.custom.is_none()));
        assert!(detect(&root.join("empty")).is_empty());
    }

    #[test]
    fn the_places_an_output_names_are_found_when_the_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "").unwrap();
        std::fs::write(root.join("b.py"), "").unwrap();
        let at = |line: &str| location(line, root).map(|l| (l.path, l.line, l.col));
        let a = root.join("src/a.rs");
        assert_eq!(at("  --> src/a.rs:12:5"), Some((a.clone(), 12, 5)));
        assert_eq!(at("src/a.rs:7: error: x"), Some((a.clone(), 7, 1)));
        assert_eq!(at("src/a.rs(3,9): error TS1"), Some((a.clone(), 3, 9)));
        assert_eq!(
            at(r#"  File "b.py", line 4, in <module>"#),
            Some((root.join("b.py"), 4, 1))
        );
        // A name that is not a file, or a time, is no place.
        assert_eq!(at("src/none.rs:12:5"), None);
        assert_eq!(at("took 12:30:45"), None);
        assert_eq!(at("nothing here"), None);
    }

    #[test]
    fn colors_and_carriage_returns_are_taken_out_of_a_line() {
        assert_eq!(strip_ansi("\x1b[1;31merror\x1b[0m: x\r"), "error: x");
        assert_eq!(strip_ansi("plain"), "plain");
    }
}
