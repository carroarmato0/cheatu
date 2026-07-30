//! cheatu — command-line memory scanner (scanmem-style REPL).

use std::io::{self, Write};

use cheatu_core::privilege;
use cheatu_core::scan::{FirstScan, NextScan, Scanner, ANY_TYPES};
use cheatu_core::{list_processes, ScanType};

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    let mut start_pid: Option<i32> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-p" | "--pid" => {
                start_pid = args.next().and_then(|s| s.parse().ok());
            }
            "-h" | "--help" => {
                print_usage();
                return;
            }
            privilege::ELEVATED_FLAG => {}
            other => eprintln!("cheatu: ignoring unknown argument {other:?}"),
        }
    }

    println!(
        "cheatu {} — Linux memory scanner",
        env!("CARGO_PKG_VERSION")
    );
    if privilege::is_root() {
        println!("running with root privileges.");
    } else {
        println!(
            "note: not running as root; reading game memory usually needs it.\n\
             use the `sudo` command to relaunch elevated, or start via `pkexec cheatu`."
        );
    }
    println!("type `help` for commands.\n");

    let mut repl = Repl::new();
    if let Some(pid) = start_pid {
        repl.attach(pid);
    }
    repl.run();
}

struct Repl {
    scanner: Option<Scanner>,
    /// The concrete type used for typed scans and direct writes.
    ty: ScanType,
    /// When set, first scans try every type in [`ANY_TYPES`].
    any: bool,
}

impl Repl {
    fn new() -> Self {
        Repl {
            scanner: None,
            ty: ScanType::I32,
            any: false,
        }
    }

    /// Types a first scan should try for the current selection.
    fn scan_types(&self) -> Vec<ScanType> {
        if self.any {
            ANY_TYPES.to_vec()
        } else {
            vec![self.ty]
        }
    }

    fn type_label(&self) -> String {
        if self.any {
            "any".to_string()
        } else {
            self.ty.label().to_string()
        }
    }

    fn run(&mut self) {
        let stdin = io::stdin();
        loop {
            print!("cheatu> ");
            let _ = io::stdout().flush();

            let mut line = String::new();
            match stdin.read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) => {}
                Err(e) => {
                    eprintln!("input error: {e}");
                    break;
                }
            }

            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if !self.dispatch(line) {
                break;
            }
        }
        println!("bye.");
    }

    /// Returns false to quit the REPL.
    fn dispatch(&mut self, line: &str) -> bool {
        let cmd = line.split_whitespace().next().unwrap_or("");
        let rest = line[cmd.len()..].trim();

        match cmd {
            "help" | "?" => print_help(),
            "quit" | "exit" | "q" => return false,
            "sudo" | "elevate" => self.elevate(),
            "ps" | "processes" => list_procs(rest),
            "pid" | "attach" => match rest.parse::<i32>() {
                Ok(pid) => self.attach(pid),
                Err(_) => println!("usage: pid <number>"),
            },
            "type" => self.set_type(rest),
            "scan" => self.first_scan(rest),
            "next" | "n" => self.next_scan(rest),
            "list" | "ls" => self.list(rest),
            "count" => self.count(),
            "set" => self.set(rest),
            "setall" => self.set_all(rest),
            "write" => self.write_direct(rest),
            "reset" | "clear" => self.reset(),
            other => println!("unknown command {other:?}; type `help`."),
        }
        true
    }

    fn elevate(&mut self) {
        if privilege::is_root() {
            println!("already running as root.");
            return;
        }
        if !privilege::pkexec_available() {
            println!("pkexec not found; install polkit or run via sudo.");
            return;
        }
        println!("requesting elevation via pkexec…");
        let err = privilege::relaunch_elevated(&[]);
        println!("could not elevate: {err}");
    }

    fn attach(&mut self, pid: i32) {
        match Scanner::new(pid) {
            Ok(s) => {
                let name = list_processes()
                    .into_iter()
                    .find(|p| p.pid == pid)
                    .map(|p| p.name)
                    .unwrap_or_else(|| "?".into());
                println!(
                    "attached to pid {pid} ({name}); scan type = {}",
                    self.type_label()
                );
                self.scanner = Some(s);
            }
            Err(e) => {
                println!("failed to attach to pid {pid}: {e}");
                if e.kind() == io::ErrorKind::PermissionDenied {
                    println!("hint: use the `sudo` command to relaunch with root access.");
                }
            }
        }
    }

    fn set_type(&mut self, rest: &str) {
        if rest.is_empty() {
            println!("current type: {}", self.type_label());
            println!(
                "available: any, {}",
                ScanType::ALL
                    .iter()
                    .map(|t| t.label())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            println!("(`any` searches every type at once — use it when you don't know the type)");
            return;
        }
        if rest.eq_ignore_ascii_case("any") {
            self.any = true;
            println!("scan type set to any (searches i32/u32/i64/u64/f32/f64 together).");
            return;
        }
        match ScanType::from_label(rest) {
            Some(t) => {
                self.any = false;
                self.ty = t;
                println!("scan type set to {t}.");
            }
            None => println!("unknown type {rest:?}."),
        }
    }

    fn first_scan(&mut self, rest: &str) {
        // Capture selection before borrowing the scanner mutably.
        let any = self.any;
        let ty = self.ty;
        let types = self.scan_types();

        let Some(scanner) = &mut self.scanner else {
            println!("not attached; use `pid <n>` first.");
            return;
        };
        if rest.is_empty() {
            println!("usage: scan <value> | scan ?   (? = unknown initial value)");
            return;
        }

        // Unknown initial value: needs a concrete type.
        if rest == "?" || rest.eq_ignore_ascii_case("unknown") {
            if any {
                println!("an unknown-value scan needs a concrete type; run e.g. `type i32` first.");
                return;
            }
            if scanner.has_scanned() {
                println!("already scanned; `scan ?` only makes sense as the first scan.");
                return;
            }
            match scanner.first_scan(FirstScan::Unknown(ty)) {
                Ok(()) => println!("{} matches.", scanner.count()),
                Err(e) => println!("scan failed: {e}"),
            }
            return;
        }

        // Value scan. If already scanned, narrow to that exact value instead.
        let result = if scanner.has_scanned() {
            scanner.next_scan(NextScan::Eq, Some(rest))
        } else {
            scanner.first_scan(FirstScan::Value {
                value: rest.to_string(),
                types,
            })
        };
        match result {
            Ok(()) => println!("{} matches.", scanner.count()),
            Err(e) => println!("scan failed: {e}"),
        }
    }

    fn next_scan(&mut self, rest: &str) {
        let Some(scanner) = &mut self.scanner else {
            println!("not attached; use `pid <n>` first.");
            return;
        };
        if !scanner.has_scanned() {
            println!("run a first `scan` before `next`.");
            return;
        }

        let mut parts = rest.split_whitespace();
        let op = parts.next().unwrap_or("");
        let operand = parts.next();

        let (cmp, arg): (NextScan, Option<&str>) = match op {
            "inc" | "+" | "increased" => (NextScan::Increased, None),
            "dec" | "-" | "decreased" => (NextScan::Decreased, None),
            "unchanged" | "same" => (NextScan::Unchanged, None),
            "changed" => (NextScan::Changed, None),
            "!=" | "<>" | "ne" => match operand {
                Some(v) => (NextScan::Ne, Some(v)),
                None => (NextScan::Changed, None),
            },
            ">" | "gt" => match operand {
                Some(v) => (NextScan::Gt, Some(v)),
                None => return println!("usage: next > <value>"),
            },
            "<" | "lt" => match operand {
                Some(v) => (NextScan::Lt, Some(v)),
                None => return println!("usage: next < <value>"),
            },
            "=" | "eq" => match operand {
                Some(v) => (NextScan::Eq, Some(v)),
                None => return println!("usage: next = <value>"),
            },
            // Bare value: `next 120`.
            other if !other.is_empty() => (NextScan::Eq, Some(other)),
            _ => return println!("usage: next <inc|dec|changed|unchanged|> N|< N|= N|!= N|VALUE>"),
        };

        match scanner.next_scan(cmp, arg) {
            Ok(()) => println!("{} matches.", scanner.count()),
            Err(e) => println!("scan failed: {e}"),
        }
    }

    fn list(&self, rest: &str) {
        let Some(scanner) = &self.scanner else {
            println!("not attached.");
            return;
        };
        let limit: usize = rest.trim().parse().unwrap_or(20);
        let total = scanner.count();
        println!("{total} candidates (showing up to {limit}):");
        for (i, cand) in scanner.results().iter().take(limit).enumerate() {
            let ty = cand.ty();
            let current = scanner
                .read_typed(cand.addr, ty)
                .map(|v| v.to_string())
                .unwrap_or_else(|_| "<unreadable>".into());
            println!(
                "  [{i:>4}] 0x{:012x}  {:>3}  was {}  now {current}",
                cand.addr,
                ty.label(),
                cand.prev
            );
        }
        if total > limit {
            println!("  … {} more (use `list <n>`).", total - limit);
        }
    }

    fn count(&self) {
        match &self.scanner {
            Some(s) => println!("{} candidates.", s.count()),
            None => println!("not attached."),
        }
    }

    fn set(&mut self, rest: &str) {
        let Some(scanner) = &self.scanner else {
            println!("not attached.");
            return;
        };
        let mut parts = rest.split_whitespace();
        let (Some(idx), Some(val)) = (parts.next(), parts.next()) else {
            println!("usage: set <index> <value>");
            return;
        };
        let Ok(idx) = idx.parse::<usize>() else {
            println!("index must be a number.");
            return;
        };
        let Some(cand) = scanner.results().get(idx) else {
            println!("no candidate at index {idx}.");
            return;
        };
        let ty = cand.ty();
        let Some(value) = ty.parse(val) else {
            println!("could not parse {val:?} as {ty}.");
            return;
        };
        match scanner.write_value(cand.addr, &value) {
            Ok(()) => println!("wrote {value} to 0x{:012x} ({ty}).", cand.addr),
            Err(e) => println!("write failed: {e}"),
        }
    }

    fn set_all(&mut self, rest: &str) {
        let Some(scanner) = &self.scanner else {
            println!("not attached.");
            return;
        };
        let val = rest.trim();
        if val.is_empty() {
            println!("usage: setall <value>");
            return;
        }
        let mut ok = 0usize;
        let mut fail = 0usize;
        for cand in scanner.results() {
            match cand.ty().parse(val) {
                Some(value) => match scanner.write_value(cand.addr, &value) {
                    Ok(()) => ok += 1,
                    Err(_) => fail += 1,
                },
                None => fail += 1,
            }
        }
        println!("wrote {val} to {ok} addresses ({fail} failed/skipped).");
    }

    fn write_direct(&mut self, rest: &str) {
        let Some(scanner) = &self.scanner else {
            println!("not attached.");
            return;
        };
        let mut parts = rest.split_whitespace();
        let (Some(addr), Some(val)) = (parts.next(), parts.next()) else {
            println!("usage: write <hex-address> <value>");
            return;
        };
        let addr = addr.trim_start_matches("0x");
        let Ok(addr) = u64::from_str_radix(addr, 16) else {
            println!("address must be hexadecimal, e.g. 0x7f1234.");
            return;
        };
        // Direct writes use the concrete type (Any has no single width).
        let Some(value) = self.ty.parse(val) else {
            println!("could not parse {val:?} as {}.", self.ty);
            return;
        };
        match scanner.write_value(addr, &value) {
            Ok(()) => println!("wrote {value} to 0x{addr:012x} ({}).", self.ty),
            Err(e) => println!("write failed: {e}"),
        }
    }

    fn reset(&mut self) {
        if let Some(s) = &mut self.scanner {
            s.reset();
            println!("candidate list cleared.");
        } else {
            println!("not attached.");
        }
    }
}

fn list_procs(filter: &str) {
    let filter = filter.trim().to_ascii_lowercase();
    let procs = list_processes();
    let mut shown = 0;
    for p in &procs {
        if !filter.is_empty()
            && !p.name.to_ascii_lowercase().contains(&filter)
            && !p.cmdline.to_ascii_lowercase().contains(&filter)
        {
            continue;
        }
        println!("{}", p.label());
        shown += 1;
    }
    if shown == 0 {
        println!("(no matching processes)");
    }
}

fn print_help() {
    println!(
        "\
commands:
  ps [filter]          list processes (optionally filtered by name/cmdline)
  pid <n>              attach to process <n>
  type [t]             show/set scan type: any i8 u8 i16 u16 i32 u32 i64 u64 f32 f64
                         `any` searches every type at once (use when the type is unknown)
  scan <value>         first scan for a value (narrows if already scanned)
  scan ?               first scan storing every address (unknown value; needs a concrete type)
  next <op> [value]    narrow results:
                         next 120        value now equals 120
                         next = 120      same as above
                         next > 50       value greater than 50
                         next < 50       value less than 50
                         next != 50      value not equal to 50
                         next inc        value increased since last scan
                         next dec        value decreased
                         next changed    value changed
                         next unchanged  value stayed the same
  list [n]             show up to n candidates with type and current value (default 20)
  count                number of candidates
  set <index> <value>  write value to candidate #index (uses that candidate's type)
  setall <value>       write value to every candidate
  write <addr> <value> write value to a hex address directly (uses the concrete type)
  reset                clear the candidate list
  sudo                 relaunch with root privileges (via pkexec)
  help                 this help
  quit                 exit"
    );
}

fn print_usage() {
    println!(
        "usage: cheatu [--pid <n>]\n\
         interactive memory scanner. run without arguments for the REPL."
    );
}
