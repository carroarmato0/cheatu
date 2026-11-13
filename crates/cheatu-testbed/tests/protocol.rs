//! The `--script` protocol, as the integration tests rely on it.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

const EXE: &str = env!("CARGO_BIN_EXE_cheatu-testbed");

struct Script {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Script {
    fn spawn() -> Script {
        let mut child = Command::new(EXE)
            .arg("--script")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Script {
            child,
            stdin,
            stdout,
        }
    }

    fn line(&mut self) -> String {
        let mut s = String::new();
        self.stdout.read_line(&mut s).unwrap();
        s.trim_end_matches('\n').to_string()
    }

    fn send(&mut self, cmd: &str) -> String {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{cmd}").unwrap();
        stdin.flush().unwrap();
        self.line()
    }

    fn ready(mut self) -> Script {
        let pid = self.child.id();
        assert_eq!(self.line(), format!("READY pid={pid}"));
        self
    }
}

impl Drop for Script {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn ready_line_has_pid() {
    Script::spawn().ready();
}

#[test]
fn replies_are_line_by_line() {
    // `send` waits for the reply while stdin stays open: an unflushed reply
    // would hang here.
    let mut s = Script::spawn().ready();
    assert_eq!(
        s.send("status"),
        "OK gold=100 kills=0 gold_shown=100 hp=100 mana=50 xp=0 ammo=10 level=1"
    );
}

#[test]
fn fight_then_get() {
    let mut s = Script::spawn().ready();
    assert!(s.send("fight").starts_with("OK gold=115 "));
    assert_eq!(s.send("get hp"), "OK 93");
    assert_eq!(s.send("get xp"), "OK 12.5");
}

#[test]
fn refusals() {
    let mut s = Script::spawn().ready();
    for _ in 0..10 {
        assert!(s.send("shoot").starts_with("OK "));
    }
    assert_eq!(s.send("shoot"), "ERR no ammo");
}

#[test]
fn death_is_reported() {
    let mut s = Script::spawn().ready();
    let mut last = String::new();
    for _ in 0..15 {
        last = s.send("fight");
    }
    assert_eq!(
        last,
        "OK died gold=100 kills=0 gold_shown=100 hp=100 mana=50 xp=0 ammo=10 level=1"
    );
}

#[test]
fn addrs_lists_all_names() {
    let mut s = Script::spawn().ready();
    let r = s.send("addrs");
    assert!(r.starts_with("OK gold=0x"), "{r}");
    for name in [
        "gold",
        "kills",
        "gold_shown",
        "hp",
        "mana",
        "xp",
        "ammo",
        "marker",
        "world",
    ] {
        assert!(r.contains(&format!(" {name}=0x")), "{name} missing: {r}");
    }
}

#[test]
fn marker_hex() {
    let mut s = Script::spawn().ready();
    assert_eq!(s.send("marker"), "OK 4348454154552d544553544245442121");
}

#[test]
fn unknowns() {
    let mut s = Script::spawn().ready();
    assert_eq!(s.send("dance"), "ERR unknown command");
    assert_eq!(s.send("get mojo"), "ERR unknown value");
    assert_eq!(s.send(""), "ERR empty");
}

#[test]
fn quit_says_bye() {
    let mut s = Script::spawn().ready();
    assert_eq!(s.send("quit"), "OK bye");
    assert!(s.child.wait().unwrap().success());
}

#[test]
fn eof_exits() {
    let mut s = Script::spawn().ready();
    s.stdin = None;
    let start = Instant::now();
    loop {
        if let Some(status) = s.child.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "still running");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn bad_arg() {
    let out = Command::new(EXE).arg("--nope").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn marker_absent_from_binary() {
    let bin = std::fs::read(EXE).unwrap();
    let needle = b"CHEATU-TESTBED!!";
    assert!(!bin.windows(needle.len()).any(|w| w == needle));
}

#[test]
fn interactive_marker_is_ready_to_paste() {
    let mut child = Command::new(EXE)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"marker\nquit\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.contains("aob(\"43 48 45 41 54 55 2d 54 45 53 54 42 45 44 21 21\")"),
        "{text}"
    );
}
