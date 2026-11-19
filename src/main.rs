//! The Tasks plugin of Ardoise: the build, test and run commands of a
//! project (Cargo, npm scripts, Make, Go, `.simpleedit/tasks.json`), run with
//! their output in a panel, the places it names a click away and its errors
//! shown in the files.

mod engine;
mod problems;
mod runner;
mod text;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use simpleedit_plugin::lint::Diagnostic;
use simpleedit_plugin::protocol::{Context, InitializeParams, Settings, UiEvent};
use simpleedit_plugin::ui::*;
use simpleedit_plugin::{run, Host, Plugin};

use engine::{Event, Location, TaskSpec};
use text::t;

const PANEL: &str = "tasks";
/// Lines of output kept; the oldest go first.
const MAX_LINES: usize = 5000;
/// Lines of output drawn (the last ones).
const SHOWN: usize = 400;
/// The panel is drawn again at most this often while a task speaks.
const REDRAW: Duration = Duration::from_millis(150);

struct OutLine {
    text: String,
    at: Option<Location>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Done {
    Code(i32),
    NoCode,
    Stopped,
}

/// What the runner thread and the plugin share.
#[derive(Default)]
struct State {
    lines: Vec<OutLine>,
    task: Option<TaskSpec>,
    /// `None` while it runs (or before the first run).
    done: Option<Done>,
    started: bool,
    /// Which run the events are from: those of an older one are dropped.
    run: u64,
    problems: BTreeMap<PathBuf, Vec<Diagnostic>>,
    /// The files whose problems changed since they were last published.
    touched: BTreeSet<PathBuf>,
    choices: Vec<TaskSpec>,
    picked: Option<String>,
    french: bool,
    last_draw: Option<Instant>,
}

struct Tasks {
    state: Arc<Mutex<State>>,
    running: Option<runner::Running>,
    active: Option<PathBuf>,
    workspace: Vec<PathBuf>,
    data_dir: PathBuf,
    /// The projects whose own `tasks.json` may run.
    trusted: BTreeSet<PathBuf>,
}

const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "Makefile",
    "makefile",
    "GNUmakefile",
    ".simpleedit",
];

/// The nearest folder above a file that looks like a project.
fn project_root(file: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let start = file.parent()?;
    let marked = start
        .ancestors()
        .take_while(|dir| Some(*dir) != home.as_deref() && dir.parent().is_some())
        .find(|dir| PROJECT_MARKERS.iter().any(|m| dir.join(m).exists()));
    Some(marked.unwrap_or(start).to_path_buf())
}

impl State {
    fn state_text(&self) -> String {
        let f = self.french;
        match self.done {
            None if self.started => t(f, "running").to_string(),
            None => String::new(),
            Some(Done::Code(0)) => t(f, "ok").to_string(),
            Some(Done::Code(code)) => format!("{} {code}", t(f, "failed")),
            Some(Done::NoCode) => t(f, "ended").to_string(),
            Some(Done::Stopped) => t(f, "stopped").to_string(),
        }
    }

    /// Adds output lines, with the places they name and the problems they say.
    fn add_lines(&mut self, raws: Vec<String>) {
        let dir = self
            .task
            .as_ref()
            .map(|t| t.dir.clone())
            .unwrap_or_default();
        let mut known: BTreeMap<PathBuf, bool> = BTreeMap::new();
        let mut is_file = |path: &Path| {
            *known
                .entry(path.to_path_buf())
                .or_insert_with(|| path.is_file())
        };
        for raw in raws {
            let text = engine::strip_ansi(&raw);
            let at = engine::location_with(&text, &dir, &mut is_file);
            if let Some(place) = &at {
                let before = self.lines.last().map(|l| l.text.as_str());
                if let Some(problem) = problems::of_line(&text, before, place) {
                    self.problems
                        .entry(place.path.clone())
                        .or_default()
                        .push(problem);
                    self.touched.insert(place.path.clone());
                }
            }
            self.lines.push(OutLine { text, at });
        }
        if self.lines.len() > MAX_LINES {
            let extra = self.lines.len() - MAX_LINES;
            self.lines.drain(..extra);
        }
    }

    fn view(&self) -> Node {
        let f = self.french;
        let names: Vec<String> = self.choices.iter().map(|t| t.name.clone()).collect();
        let running = self.started && self.done.is_none();
        let mut head = vec![
            select("task", names, self.picked.clone()).placeholder(t(f, "pick")),
            button(t(f, "run"), "run")
                .button_style(ButtonStyle::Primary)
                .enabled(self.picked.is_some() && !running),
            button(t(f, "stop"), "stop")
                .button_style(ButtonStyle::Danger)
                .enabled(running),
            button(t(f, "rerun"), "rerun").enabled(self.task.is_some() && !running),
            button(t(f, "clear"), "clear").button_style(ButtonStyle::Ghost),
        ];
        let state = self.state_text();
        if !state.is_empty() {
            let name = self
                .task
                .as_ref()
                .map(|t| t.name.clone())
                .unwrap_or_default();
            head.push(text(format!("{name}: {state}")).style(match self.done {
                Some(Done::Code(0)) | None => TextStyle::Muted,
                _ => TextStyle::Error,
            }));
        }
        if self.choices.is_empty() {
            head.push(text(t(f, "none")).style(TextStyle::Muted));
        }
        let first = self.lines.len().saturating_sub(SHOWN);
        let lines = self.lines[first..]
            .iter()
            .enumerate()
            .map(|(i, line)| match &line.at {
                Some(_) => button(line.text.clone(), format!("open:{}", first + i))
                    .button_style(ButtonStyle::Ghost),
                None => text(line.text.clone()).style(TextStyle::Mono),
            });
        column([
            row(head).spacing(8.0),
            scroll(column(lines).spacing(0.0)).stick_to_bottom(),
        ])
        .spacing(6.0)
    }

    /// Draws the panel and publishes the problems that changed, unless it
    /// was done a moment ago (`force`: whatever the time).
    fn draw(&mut self, host: &Host, force: bool) {
        if !force && self.last_draw.is_some_and(|d| d.elapsed() < REDRAW) {
            return;
        }
        self.last_draw = Some(Instant::now());
        for path in std::mem::take(&mut self.touched) {
            let list = self.problems.get(&path).cloned().unwrap_or_default();
            host.publish_diagnostics(&path, list);
        }
        host.render(PANEL, self.view());
    }
}

impl Tasks {
    fn new() -> Self {
        Tasks {
            state: Arc::default(),
            running: None,
            active: None,
            workspace: Vec::new(),
            data_dir: PathBuf::new(),
            trusted: BTreeSet::new(),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The folder whose tasks are offered: the project of the file in front,
    /// else the first project.
    fn root(&self) -> Option<PathBuf> {
        self.active
            .as_deref()
            .and_then(project_root)
            .or_else(|| self.workspace.first().cloned())
    }

    fn refresh_choices(&self) {
        let choices = self.root().map(|r| engine::detect(&r)).unwrap_or_default();
        let mut s = self.state();
        if s.picked
            .as_ref()
            .is_some_and(|p| !choices.iter().any(|c| &c.name == p))
        {
            s.picked = None;
        }
        if s.picked.is_none() {
            s.picked = choices.first().map(|c| c.name.clone());
        }
        s.choices = choices;
    }

    fn trusted_file(&self) -> PathBuf {
        self.data_dir.join("trusted.json")
    }

    /// A command the project wrote itself runs once the user trusts it.
    fn may_run(&mut self, host: &Host, task: &TaskSpec) -> bool {
        let Some(root) = &task.custom else {
            return true;
        };
        if self.trusted.contains(root) {
            return true;
        }
        let french = self.state().french;
        let question = format!(
            "{} {}",
            t(french, "trust"),
            root.join(".simpleedit/tasks.json").display()
        );
        if !host.confirm(question).unwrap_or(false) {
            return false;
        }
        self.trusted.insert(root.clone());
        if let Ok(json) = serde_json::to_string_pretty(&self.trusted) {
            let _ = std::fs::write(self.trusted_file(), json);
        }
        true
    }

    fn start(&mut self, host: &Host, task: TaskSpec) {
        if !self.may_run(host, &task) {
            return;
        }
        if let Some(mut old) = self.running.take() {
            old.stop();
        }
        let run = {
            let mut s = self.state();
            s.run += 1;
            s.lines.clear();
            // The problems of the last run go.
            let old: Vec<PathBuf> = s.problems.keys().cloned().collect();
            s.problems.clear();
            s.touched.extend(old);
            s.done = None;
            s.started = true;
            s.task = Some(task.clone());
            s.draw(host, true);
            s.run
        };
        let (state, host_for_run) = (self.state.clone(), host.clone());
        self.running = Some(runner::start(&task, move |event| {
            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
            if s.run != run {
                return;
            }
            match event {
                Event::Line(line) => s.add_lines(vec![line]),
                Event::Lines(lines) => s.add_lines(lines),
                Event::Done(code) => {
                    if s.done.is_none() {
                        s.done = Some(code.map_or(Done::NoCode, Done::Code));
                    }
                    let name = s.task.as_ref().map(|t| t.name.clone()).unwrap_or_default();
                    let said = format!("{name}: {}", s.state_text());
                    if code == Some(0) {
                        host_for_run.notify(said);
                    } else {
                        host_for_run.error(said);
                    }
                    s.draw(&host_for_run, true);
                    return;
                }
            }
            s.draw(&host_for_run, false);
        }));
    }

    fn stop(&mut self, host: &Host) {
        if let Some(mut running) = self.running.take() {
            running.stop();
            let mut s = self.state();
            if s.done.is_none() {
                s.done = Some(Done::Stopped);
            }
            s.draw(host, true);
        }
    }

    fn picked_task(&self) -> Option<TaskSpec> {
        let s = self.state();
        let name = s.picked.clone()?;
        s.choices.iter().find(|c| c.name == name).cloned()
    }

    fn show(&mut self, host: &Host) {
        self.refresh_choices();
        host.show_panel(PANEL);
        self.state().draw(host, true);
    }
}

impl Plugin for Tasks {
    fn initialize(&mut self, _: &Host, params: &InitializeParams) {
        self.workspace = params.workspace.clone();
        self.data_dir = params.data_dir.clone();
        self.state().french = params.settings.locale.starts_with("fr");
        self.trusted = std::fs::read_to_string(self.trusted_file())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
    }

    fn command(&mut self, host: &Host, command: &str, context: &Context) {
        if let Some(file) = &context.file {
            self.active = Some(file.clone());
        }
        if !context.workspace.is_empty() {
            self.workspace = context.workspace.clone();
        }
        match command {
            "tasks.run" | "tasks.show" => self.show(host),
            "tasks.rerun" => {
                let last = self.state().task.clone();
                match last.or_else(|| self.picked_task()) {
                    Some(task) => self.start(host, task),
                    None => self.show(host),
                }
            }
            "tasks.stop" => self.stop(host),
            _ => {}
        }
    }

    fn panel_opened(&mut self, host: &Host, panel: &str) {
        if panel == PANEL {
            self.refresh_choices();
            self.state().draw(host, true);
        }
    }

    fn ui_event(&mut self, host: &Host, event: &UiEvent) {
        match event.action.as_str() {
            "task" => self.state().picked = event.text().map(str::to_string),
            "run" => {
                if let Some(task) = self.picked_task() {
                    self.start(host, task);
                }
                return;
            }
            "rerun" => {
                // The lock goes before the run takes it again.
                let last = self.state().task.clone();
                if let Some(task) = last {
                    self.start(host, task);
                }
                return;
            }
            "stop" => {
                self.stop(host);
                return;
            }
            "clear" => {
                let mut s = self.state();
                s.lines.clear();
            }
            action => {
                let at = action
                    .strip_prefix("open:")
                    .and_then(|i| i.parse::<usize>().ok())
                    .and_then(|i| self.state().lines.get(i).and_then(|l| l.at.clone()));
                if let Some(at) = at {
                    let _ = host.open_file(&at.path, Some(at.line));
                }
                return;
            }
        }
        self.state().draw(host, true);
    }

    fn active_file_changed(&mut self, _: &Host, path: Option<&Path>) {
        if let Some(path) = path {
            self.active = Some(path.to_path_buf());
        }
    }

    fn settings_changed(&mut self, host: &Host, settings: &Settings) {
        let mut s = self.state();
        s.french = settings.locale.starts_with("fr");
        s.draw(host, true);
    }

    fn shutdown(&mut self, _: &Host) {
        if let Some(mut running) = self.running.take() {
            running.stop_now();
        }
    }
}

fn main() -> std::io::Result<()> {
    run(Tasks::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_lines_keep_their_places_and_problems() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        let mut s = State {
            task: Some(TaskSpec {
                name: "cargo build".into(),
                command: "x".into(),
                dir: dir.path().to_path_buf(),
                custom: None,
            }),
            started: true,
            ..State::default()
        };
        s.add_lines(vec![
            "\x1b[1;31merror\x1b[0m: mismatched types".into(),
            "  --> a.rs:1:4".into(),
            "nothing here".into(),
        ]);
        assert_eq!(s.lines[0].text, "error: mismatched types");
        assert!(s.lines[1].at.is_some() && s.lines[2].at.is_none());
        assert_eq!(s.problems[&file].len(), 1);
        assert!(s.touched.contains(&file));
        let page = serde_json::to_string(&s.view()).unwrap();
        assert!(page.contains("open:1") && page.contains("Running"));
        s.done = Some(Done::Code(101));
        assert_eq!(s.state_text(), "Failed with code 101");
    }

    #[test]
    fn the_project_of_the_file_in_front_gives_its_tasks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let mut tasks = Tasks::new();
        tasks.active = Some(dir.path().join("src/main.rs"));
        tasks.refresh_choices();
        let s = tasks.state();
        assert!(s.choices.iter().any(|t| t.name == "cargo build"));
        assert_eq!(s.picked.as_deref(), Some("cargo build"));
    }
}
