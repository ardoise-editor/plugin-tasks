//! Running a task: `sh -c` in a process group of its own (a stop reaches
//! what the shell started too), both outputs line by line, then the code.

use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::mpsc;

use crate::engine::{Event, TaskSpec};

/// The most lines sent in one packet.
const PACKET: usize = 200;

/// A task that runs: stopping it ends its process group.
pub struct Running {
    #[cfg(unix)]
    group: Option<i32>,
}

impl Running {
    /// Asks the group to end, then forces it.
    pub fn stop(&mut self) {
        #[cfg(unix)]
        if let Some(id) = self.group.take() {
            std::thread::spawn(move || end_group(id, std::time::Duration::from_secs(1)));
        }
    }

    /// Ends it at once (the plugin is stopping).
    pub fn stop_now(&mut self) {
        #[cfg(unix)]
        if let Some(id) = self.group.take() {
            end_group(id, std::time::Duration::from_millis(300));
        }
    }
}

#[cfg(unix)]
fn end_group(id: i32, grace: std::time::Duration) {
    // SAFETY: `killpg` with a group id and a signal has no other effect.
    let alive = || unsafe { libc::killpg(id, 0) } == 0;
    unsafe { libc::killpg(id, libc::SIGTERM) };
    let start = std::time::Instant::now();
    while alive() && start.elapsed() < grace {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if alive() {
        unsafe { libc::killpg(id, libc::SIGKILL) };
    }
}

fn forward(reader: impl Read + Send + 'static, to: mpsc::Sender<String>) {
    std::thread::spawn(move || {
        // Bytes, not lines of text: an invalid UTF-8 byte must not end the
        // output (and close the pipe under the command).
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            if !matches!(reader.read_until(b'\n', &mut buf), Ok(n) if n > 0) {
                break;
            }
            if buf.last() == Some(&b'\n') {
                buf.pop();
                if buf.last() == Some(&b'\r') {
                    buf.pop();
                }
            }
            if to.send(String::from_utf8_lossy(&buf).into_owned()).is_err() {
                break;
            }
        }
    });
}

/// Starts the task; `on` hears its output in packets, then its end, on a
/// thread of its own.
pub fn start(task: &TaskSpec, mut on: impl FnMut(Event) + Send + 'static) -> Running {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(&task.command)
        .current_dir(&task.dir)
        // The messages of compilers in English: the panel looks for `error`
        // and `warning`. Only the messages: the encoding stays.
        .env("LC_MESSAGES", "C")
        .env_remove("LANGUAGE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            let line = format!("{}: {e}", task.command);
            std::thread::spawn(move || {
                on(Event::Line(line));
                on(Event::Done(None));
            });
            return Running {
                #[cfg(unix)]
                group: None,
            };
        }
    };
    let running = Running {
        #[cfg(unix)]
        group: Some(child.id() as i32),
    };
    let (to, from) = mpsc::channel();
    if let Some(out) = child.stdout.take() {
        forward(out, to.clone());
    }
    if let Some(err) = child.stderr.take() {
        forward(err, to.clone());
    }
    drop(to);
    std::thread::spawn(move || {
        // What is already there goes in one packet.
        while let Ok(line) = from.recv() {
            let mut packet = vec![line];
            while packet.len() < PACKET {
                match from.try_recv() {
                    Ok(line) => packet.push(line),
                    Err(_) => break,
                }
            }
            on(Event::Lines(packet));
        }
        let code = child.wait().ok().and_then(|s| s.code());
        on(Event::Done(code));
    });
    running
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    fn spec(command: &str, dir: &std::path::Path) -> TaskSpec {
        TaskSpec {
            name: "t".into(),
            command: command.into(),
            dir: dir.to_path_buf(),
            custom: None,
        }
    }

    /// Runs and waits for the end: the lines, the code.
    fn run(command: &str) -> (Vec<String>, Option<i32>) {
        let dir = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new((Vec::new(), None, false)));
        let out = seen.clone();
        let _running = start(&spec(command, dir.path()), move |e| {
            let mut s = out.lock().unwrap();
            match e {
                Event::Line(l) => s.0.push(l),
                Event::Lines(ls) => s.0.extend(ls),
                Event::Done(code) => {
                    s.1 = code;
                    s.2 = true;
                }
            }
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        while !seen.lock().unwrap().2 {
            assert!(Instant::now() < deadline, "the task never ended");
            std::thread::sleep(Duration::from_millis(10));
        }
        let s = seen.lock().unwrap();
        (s.0.clone(), s.1)
    }

    #[test]
    fn both_outputs_then_the_exit_code() {
        let (mut lines, code) = run("echo out; echo err 1>&2; exit 3");
        lines.sort();
        assert_eq!(lines, vec!["err".to_string(), "out".to_string()]);
        assert_eq!(code, Some(3));
    }

    #[test]
    fn a_byte_that_is_not_utf8_does_not_end_the_output() {
        let (lines, code) = run("printf 'a\\351b\\nafter\\n'");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1], "after");
        assert_eq!(code, Some(0));
    }

    #[test]
    fn a_task_speaks_english() {
        let (lines, _) = run("echo $LC_MESSAGES");
        assert_eq!(lines, vec!["C".to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn a_stop_ends_what_the_command_started_too() {
        let dir = tempfile::tempdir().unwrap();
        let done = Arc::new(Mutex::new(false));
        let flag = done.clone();
        let mut running = start(&spec("sleep 30 & sleep 30; wait", dir.path()), move |e| {
            if let Event::Done(_) = e {
                *flag.lock().unwrap() = true;
            }
        });
        std::thread::sleep(Duration::from_millis(200));
        running.stop_now();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !*done.lock().unwrap() {
            assert!(Instant::now() < deadline, "the group was not ended");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
