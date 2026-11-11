//! cheatu — command-line memory scanner (scanmem-style REPL).

use std::io::{self, Write};

mod ptr_cmds;
mod table_cmds;

use cheatu_core::pointer_scan::PtrResults;
use cheatu_core::privilege;
use cheatu_core::scan::{FirstScan, NextScan, Scanner, ANY_TYPES};
use cheatu_core::table::TableEntry;
use cheatu_core::LiveSource;
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

    let os = if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Linux"
    };
    println!("cheatu {} — {os} memory scanner", env!("CARGO_PKG_VERSION"));
    if privilege::is_root() {
        println!("running with root privileges.");
    } else if cfg!(target_os = "macos") {
        println!(
            "note: not running as root; reading game memory usually needs it.\n\
             restart with `sudo cheatu` to scan other processes."
        );
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
    /// Expression resolution and pointer scans read through this.
    src: Option<LiveSource>,
    /// Name of the attached process, for saved tables and pointer results.
    process: Option<String>,
    table: Vec<TableEntry>,
    ptr: Option<PtrResults>,
}

impl Repl {
    fn new() -> Self {
        Repl {
            scanner: None,
            ty: ScanType::I32,
            any: false,
            src: None,
            process: None,
            table: Vec::new(),
            ptr: None,
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
            if !self.dispatch(line, &mut io::stdout()) {
                break;
            }
        }
        println!("bye.");
    }

    /// Returns false to quit the REPL.
    fn dispatch(&mut self, line: &str, out: &mut dyn Write) -> bool {
        let _ = self.dispatch_io(line, out);
        !matches!(line.split_whitespace().next(), Some("quit" | "exit" | "q"))
    }

    /// The memory source with a fresh memory map, so modules loaded since
    /// attaching resolve.
    fn fresh_src(&mut self) -> Option<&LiveSource> {
        if let Some(s) = &mut self.src {
            let _ = s.refresh_regions();
        }
        self.src.as_ref()
    }

    fn dispatch_io(&mut self, line: &str, out: &mut dyn Write) -> io::Result<()> {
        let cmd = line.split_whitespace().next().unwrap_or("");
        let rest = line[cmd.len()..].trim();

        match cmd {
            "help" | "?" => print_help(out)?,
            "quit" | "exit" | "q" => {}
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
            "write" => self.write_direct(rest, out)?,
            "add" => {
                let ty = self.ty;
                self.fresh_src();
                table_cmds::add(out, &mut self.table, self.src.as_ref(), ty, rest)?
            }
            "entries" => {
                self.fresh_src();
                table_cmds::entries(out, &self.table, self.src.as_ref())?
            }
            "resolve" => {
                self.fresh_src();
                table_cmds::resolve(out, self.src.as_ref(), rest)?
            }
            "save" => table_cmds::save(out, &self.table, self.process.as_deref(), rest)?,
            "load" => table_cmds::load(out, &mut self.table, rest)?,
            "ptrscan" => {
                let ctx = ptr_cmds::PtrCtx {
                    src: self.src.as_mut(),
                    process: self.process.as_deref().unwrap_or("?"),
                    ty: self.ty,
                };
                ptr_cmds::run(out, &mut self.ptr, ctx, rest)?
            }
            "reset" | "clear" => self.reset(),
            other => println!("unknown command {other:?}; type `help`."),
        }
        Ok(())
    }

    fn elevate(&mut self) {
        if privilege::is_root() {
            println!("already running as root.");
            return;
        }
        // An elevated relaunch from the admin prompt would run detached from
        // this terminal, so on macOS the CLI is restarted under sudo instead.
        if cfg!(target_os = "macos") {
            println!("on macOS, restart cheatu with: sudo cheatu");
            return;
        }
        if !privilege::elevation_available() {
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
                self.process = Some(name);
                self.src = match LiveSource::open(pid) {
                    Ok(src) => Some(src),
                    Err(e) => {
                        println!("note: address expressions unavailable: {e}");
                        None
                    }
                };
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
        let types = self.scan_types();

        let Some(scanner) = &mut self.scanner else {
            println!("not attached; use `pid <n>` first.");
            return;
        };
        if rest.is_empty() {
            println!("usage: scan <value> | scan ?   (? = unknown initial value)");
            return;
        }

        // Unknown initial value. With `type any` this stores every address once
        // per type, which only fits for modest processes — the engine says so
        // rather than eating all the memory.
        if rest == "?" || rest.eq_ignore_ascii_case("unknown") {
            if scanner.has_scanned() {
                println!("already scanned; `scan ?` only makes sense as the first scan.");
                return;
            }
            match scanner.first_scan(FirstScan::Unknown(types)) {
                Ok(()) => println!("{} matches.", scanner.count()),
                Err(e) => println!("scan failed: {e}"),
            }
            return;
        }

        // A range, for a value you can only bracket ("gold is a bit over 6.4M").
        if let Some((low, high)) = cheatu_core::parse_range(rest) {
            if scanner.has_scanned() {
                println!(
                    "a range only works as a first scan; use `next > {low}` then `next < {high}`."
                );
                return;
            }
            match scanner.first_scan(FirstScan::Range { low, high, types }) {
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

    fn write_direct(&mut self, rest: &str, out: &mut dyn Write) -> io::Result<()> {
        self.fresh_src();
        let (Some(scanner), Some(src)) = (&self.scanner, &self.src) else {
            return writeln!(out, "not attached.");
        };
        let (addr, val) = table_cmds::split_expr(rest);
        let val = val.split_whitespace().next().unwrap_or("");
        if addr.is_empty() || val.is_empty() {
            return writeln!(out, "usage: write <expr|hex-address> <value>");
        }
        // Digits only keeps the old meaning (hex, `0x` optional); anything
        // else is an address expression.
        let addr = match table_cmds::parse_target(src, addr) {
            Ok(a) => a,
            Err(e) => return writeln!(out, "{e}"),
        };
        // Direct writes use the concrete type (Any has no single width).
        let Some(value) = self.ty.parse(val) else {
            return writeln!(out, "could not parse {val:?} as {}.", self.ty);
        };
        match scanner.write_value(addr, &value) {
            Ok(()) => writeln!(out, "wrote {value} to 0x{addr:012x} ({}).", self.ty),
            Err(e) => writeln!(out, "write failed: {e}"),
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

fn print_help(out: &mut dyn Write) -> io::Result<()> {
    writeln!(
        out,
        "\
commands:
  ps [filter]          list processes (optionally filtered by name/cmdline)
  pid <n>              attach to process <n>
  type [t]             show/set scan type: any i8 u8 i16 u16 i32 u32 i64 u64 f32 f64
                         `any` searches every type at once (use when the type is unknown)
  scan <value>         first scan for a value (narrows if already scanned)
  scan <low>..<high>   first scan for any value in a range, e.g. scan 6400000..6500000
                         (for a number you can only bracket, not read exactly)
  scan ?               first scan storing every address (unknown value; works with `type any` too)
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
  write <expr|hex> <value>
                       write value to an address (hex digits, or an expression) directly
                         (uses the concrete type)
  add <expr> [type] [description…]
                       add an address expression to the cheat table
  entries              show table entries with resolved address and current value
  resolve <expr>       resolve an address expression, e.g. resolve [\"game\"+0x10]+0x8
  save <file>          save the cheat table
  load <file>          load a cheat table (never writes memory; freezes stay off)
  ptrscan <expr> [depth] [max-offset]
                       find pointer chains from a module to an address (hex needs 0x)
                         (depth 1-8, default 5; max-offset 0x8-0x100000, default 0x1000)
  ptrscan filter addr <expr>
                       keep chains that resolve to this address now
  ptrscan filter value <value>
                       keep chains whose target holds this value now (current type)
  ptrscan save <file>  save the pointer scan results
  ptrscan load <file>  load pointer scan results
  ptrscan list [n]     show up to n chains (default 20)
  reset                clear the candidate list
  {}
  help                 this help
  quit                 exit",
        if cfg!(target_os = "macos") {
            "sudo                 explain how to run with root privileges"
        } else {
            "sudo                 relaunch with root privileges (via pkexec)"
        }
    )
}

fn print_usage() {
    println!(
        "usage: cheatu [--pid <n>]\n\
         interactive memory scanner. run without arguments for the REPL."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

    static PTR: AtomicUsize = AtomicUsize::new(0);

    fn attached() -> Repl {
        let mut repl = Repl::new();
        let mut out = Vec::new();
        repl.dispatch(&format!("attach {}", std::process::id()), &mut out);
        assert!(repl.scanner.is_some(), "attach failed");
        repl
    }

    fn run(repl: &mut Repl, line: &str) -> String {
        let mut out = Vec::new();
        repl.dispatch(line, &mut out);
        String::from_utf8(out).unwrap()
    }

    fn leaked_u32(v: u32) -> (&'static AtomicU32, u64) {
        let b: &'static AtomicU32 = Box::leak(Box::new(AtomicU32::new(v)));
        (b, b as *const AtomicU32 as u64)
    }

    #[test]
    fn add_and_entries_show_resolved_value() {
        let (_b, addr) = leaked_u32(0x00C0_FFEE);
        let mut repl = attached();
        let out = run(&mut repl, &format!("add {addr:#x} u32 my gold"));
        assert!(out.contains("added entry 0"), "{out}");
        let out = run(&mut repl, "entries");
        assert!(out.contains("my gold"), "{out}");
        assert!(out.contains(&format!("{addr:#x}")), "{out}");
        assert!(out.contains("12648430"), "{out}");
    }

    #[test]
    fn save_then_load_round_trips_with_freezes_off() {
        let (_b, addr) = leaked_u32(5);
        let mut repl = attached();
        run(&mut repl, &format!("add {addr:#x} u32 hp"));
        repl.table[0].frozen = true;
        let path = std::env::temp_dir().join(format!("cheatu-cli-{}.toml", std::process::id()));
        let p = path.display();
        assert!(run(&mut repl, &format!("save {p}")).contains("saved 1"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("frozen = true"), "{text}");
        repl.table.clear();
        let out = run(&mut repl, &format!("load {p}"));
        std::fs::remove_file(&path).ok();
        assert!(out.contains("loaded 1 entries, freezes off"), "{out}");
        assert_eq!(repl.table.len(), 1);
        assert!(!repl.table[0].frozen);
        assert_eq!(repl.table[0].description, "hp");
    }

    #[test]
    fn resolve_reports_error_text() {
        let mut repl = attached();
        let out = run(&mut repl, r#"resolve "no_such_module"+0x10"#);
        assert!(out.contains("no_such_module"), "{out}");
        assert!(!out.contains("= 0x"), "{out}");
    }

    #[test]
    fn write_accepts_legacy_hex() {
        let (b, addr) = leaked_u32(1);
        let mut repl = attached();
        run(&mut repl, "type u32");
        let out = run(&mut repl, &format!("write {addr:x} 77"));
        assert!(out.contains("wrote 77"), "{out}");
        assert_eq!(b.load(Ordering::SeqCst), 77);
        let out = run(&mut repl, &format!("write 0x{addr:x} 78"));
        assert!(out.contains("wrote 78"), "{out}");
        assert_eq!(b.load(Ordering::SeqCst), 78);
    }

    #[test]
    fn ptrscan_finds_chain_and_lists_it() {
        let target: &'static u64 = Box::leak(Box::new(0x1234_5678u64));
        let addr = target as *const u64 as u64;
        PTR.store(addr as usize, Ordering::SeqCst);
        let mut repl = attached();
        let out = run(&mut repl, &format!("ptrscan {addr:#x} 1"));
        assert!(out.contains("chains found"), "{out}");
        let res = repl.ptr.as_ref().expect("results stored");
        let src = repl.src.as_ref().unwrap();
        let chain = res
            .chains
            .iter()
            .find(|c| c.resolve(src) == Ok(addr))
            .expect("chain through PTR");
        let expr = chain.to_expr().to_string();
        let out = run(&mut repl, "ptrscan list 1000000");
        assert!(out.contains(&expr), "{expr} not in listing");
    }

    #[test]
    fn ptrscan_targets_are_expressions_not_bare_hex() {
        let mut repl = attached();
        let out = run(&mut repl, "ptrscan 7ff0 1");
        assert!(out.contains("bad expression"), "{out}");
        assert!(!out.contains("pointer scan for"), "{out}");

        repl.ptr = Some(cheatu_core::pointer_scan::PtrResults {
            process: "self".into(),
            target: 0,
            settings: Default::default(),
            chains: Vec::new(),
            truncated: false,
        });
        let out = run(&mut repl, "ptrscan filter addr 7ff0");
        assert!(out.contains("bad expression"), "{out}");
        let out = run(&mut repl, "ptrscan filter addr 4096");
        assert!(out.contains("0 chains remain"), "{out}");
        assert!(run(&mut repl, "ptrscan").contains("ptrscan <expr> "));
    }

    #[test]
    fn ptrscan_validates_arguments() {
        let mut repl = attached();
        assert!(run(&mut repl, "ptrscan 0x10 9").contains("depth"));
        assert!(run(&mut repl, "ptrscan 0x10 2 4").contains("max-offset"));
        assert!(run(&mut repl, "ptrscan list").contains("no pointer scan results"));
    }
}
