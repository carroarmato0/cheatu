//! Spawn the testbed in `--script` mode and attach cheatu-core to it.
#![allow(dead_code)] // each test file uses a different part

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use cheatu_core::LiveSource;

pub const EXE: &str = env!("CARGO_BIN_EXE_cheatu-testbed");
pub const MODULE: &str = "cheatu-testbed";

pub struct Testbed {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

/// On macOS, sign the testbed with get-task-allow so a non-root test may
/// attach (see the spec's Measurements section). Once per test process.
fn prepare() {
    #[cfg(target_os = "macos")]
    {
        static SIGN: std::sync::Once = std::sync::Once::new();
        SIGN.call_once(|| {
            let plist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("entitlements.plist");
            let status = Command::new("codesign")
                .args(["--force", "--sign", "-", "--entitlements"])
                .arg(&plist)
                .arg(EXE)
                .stderr(Stdio::null())
                .status()
                .expect("run codesign");
            assert!(status.success(), "codesign failed");
        });
    }
}

impl Testbed {
    pub fn spawn() -> Testbed {
        prepare();
        let mut child = Command::new(EXE)
            .arg("--script")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn cheatu-testbed");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut tb = Testbed {
            child,
            stdin,
            stdout,
        };
        let ready = tb.line();
        assert_eq!(ready, format!("READY pid={}", tb.child.id()));
        tb
    }

    pub fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    fn line(&mut self) -> String {
        let mut s = String::new();
        self.stdout.read_line(&mut s).expect("read from testbed");
        assert!(!s.is_empty(), "testbed closed its output");
        s.trim_end_matches('\n').to_string()
    }

    /// Send `line` and return the reply as is.
    pub fn cmd_raw(&mut self, line: &str) -> String {
        writeln!(self.stdin, "{line}").expect("write to testbed");
        self.stdin.flush().unwrap();
        self.line()
    }

    /// Send `line`, require `OK`, and return what follows it.
    pub fn cmd(&mut self, line: &str) -> String {
        let r = self.cmd_raw(line);
        match r.strip_prefix("OK") {
            Some(rest) => rest.trim_start().to_string(),
            None => panic!("`{line}` failed: {r}"),
        }
    }

    /// The true current address of `name`, from `addrs`.
    pub fn addr(&mut self, name: &str) -> u64 {
        let all = self.cmd("addrs");
        let hex = all
            .split(' ')
            .find_map(|kv| kv.strip_prefix(&format!("{name}=0x")))
            .unwrap_or_else(|| panic!("no {name} in {all}"));
        u64::from_str_radix(hex, 16).unwrap()
    }

    /// `name`'s value as the game reads it.
    pub fn get(&mut self, name: &str) -> String {
        self.cmd(&format!("get {name}"))
    }
}

impl Drop for Testbed {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Open the testbed's memory, or `None` (test skipped) without permission.
/// With `CHEATU_TESTBED_REQUIRE=1` a missing permission fails instead.
pub fn attach(tb: &Testbed) -> Option<LiveSource> {
    match LiveSource::open(tb.pid()) {
        Ok(src) => Some(src),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            if std::env::var("CHEATU_TESTBED_REQUIRE").as_deref() == Ok("1") {
                panic!("cannot attach and CHEATU_TESTBED_REQUIRE=1: {e}");
            }
            eprintln!("skipped: no permission to attach ({e})");
            None
        }
        Err(e) => panic!("attach to testbed: {e}"),
    }
}

/// Write `bytes` at `addr` in the testbed.
pub fn write(tb: &Testbed, addr: u64, bytes: &[u8]) {
    cheatu_core::Mem::open(tb.pid())
        .and_then(|m| m.write_at(addr, bytes))
        .expect("write to testbed");
}
