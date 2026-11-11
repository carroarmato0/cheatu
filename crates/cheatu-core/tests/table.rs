//! Cheat table file format: round trips, refusals, limits, atomic save.

use cheatu_core::table::{
    load, parse, save, to_toml, CheatTable, TableEntry, MAX_ENTRIES, MAX_FILE_BYTES,
};
use cheatu_core::{AddrExpr, Expr, ScanType};
use std::fs;

const FIXTURE: &str = include_str!("fixtures/rupees.toml");

fn err(text: &str) -> cheatu_core::table::TableError {
    parse(text).expect_err("expected refusal")
}

fn entry_text(extra: &str) -> String {
    format!("version = 1\n\n[[entry]]\ndescription = \"x\"\naddress = '\"m\"+1'\n{extra}\n")
}

#[test]
fn fixture_round_trips() {
    let t = parse(FIXTURE).unwrap();
    let again = parse(&to_toml(&t).unwrap()).unwrap();
    assert_eq!(t, again);
    assert_eq!(t.process.as_deref(), Some("dmhost"));
    assert_eq!(t.entries.len(), 1);
    assert_eq!(t.entries[0].description, "Rupees");
    assert_eq!(t.entries[0].ty, ScanType::I16);
}

#[test]
fn frozen_is_read_as_saved() {
    assert!(parse(FIXTURE).unwrap().entries[0].frozen);
}

#[test]
fn crlf_loads() {
    let t = parse(&FIXTURE.replace('\n', "\r\n")).unwrap();
    assert_eq!(t.entries.len(), 1);
}

#[test]
fn bytes_round_trip_and_alias_accepted() {
    let t = parse(&entry_text("type = \"bytes\"\nvalue = \"DE AD be ef\"")).unwrap();
    assert_eq!(t.entries[0].ty, ScanType::Bytes(4));
    assert_eq!(parse(&to_toml(&t).unwrap()).unwrap(), t);
    let t = parse(&entry_text("type = \"int\"\nvalue = \"5\"")).unwrap();
    assert_eq!(t.entries[0].ty, ScanType::I32);
    assert!(to_toml(&t).unwrap().contains("type = \"i32\""));
}

#[test]
fn unknown_key_refused_with_line() {
    let e = err("version = 1\ndescripton = \"x\"\n");
    assert_eq!(e.line, Some(2));
    let e = err(&entry_text(
        "type = \"i8\"\nvalue = \"1\"\ndescripton = \"x\"",
    ));
    assert_eq!(e.line, Some(8));
    assert!(e.message.contains("descripton"), "{}", e.message);
}

#[test]
fn unknown_type_refused() {
    let e = err(&entry_text("type = \"i33\"\nvalue = \"1\""));
    assert_eq!(e.line, Some(3));
    assert!(e.message.contains("unknown type"), "{}", e.message);
}

#[test]
fn value_out_of_range_refused() {
    let e = err(&entry_text("type = \"i8\"\nvalue = \"300\""));
    assert_eq!(e.line, Some(3));
    assert!(e.message.contains("does not fit i8"), "{}", e.message);
    let e = err(&entry_text("type = \"i8\"\nvalue = \"abc\""));
    assert!(e.message.contains("value"), "{}", e.message);
}

#[test]
fn bad_expression_refused() {
    let text = "version = 1\n\n[[entry]]\ndescription = \"x\"\naddress = '1 +'\ntype = \"i8\"\nvalue = \"1\"\n";
    let e = err(text);
    assert_eq!(e.line, Some(3));
    assert!(e.message.starts_with("address:"), "{}", e.message);
}

#[test]
fn version_checks() {
    let e = err("process = \"x\"\n");
    assert!(e.message.contains("version"), "{}", e.message);
    let e = err("version = 2\n");
    assert!(e.message.contains("unsupported version 2"), "{}", e.message);
    assert_eq!(e.line, Some(1));
}

#[test]
fn syntax_error_has_line() {
    let e = err("version = 1\n[[entry\n");
    assert_eq!(e.line, Some(2));
}

#[test]
fn display_has_line_prefix() {
    let e = err("version = 2\n");
    assert!(e.to_string().starts_with("line 1: "), "{e}");
}

#[test]
fn too_large_file_refused() {
    let dir = tempdir("large");
    let p = dir.join("big.toml");
    let mut text = String::from("version = 1\n#");
    text.push_str(&"a".repeat(MAX_FILE_BYTES as usize));
    fs::write(&p, &text).unwrap();
    let e = load(&p).unwrap_err();
    assert!(e.message.contains("too large"), "{}", e.message);
    assert!(parse(&text).unwrap_err().message.contains("too large"));
}

#[test]
fn too_many_entries_refused() {
    let mut text = String::from("version = 1\n");
    for _ in 0..=MAX_ENTRIES {
        text.push_str("[[entry]]\ndescription=\"\"\naddress=\"1\"\ntype=\"i8\"\nvalue=\"1\"\n");
    }
    assert!(text.len() < MAX_FILE_BYTES as usize);
    let e = err(&text);
    assert!(e.message.contains("too many entries"), "{}", e.message);
}

fn tempdir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("cheatu-table-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn save_is_atomic() {
    let dir = tempdir("save");
    let p = dir.join("t.toml");
    let table = CheatTable {
        process: None,
        entries: vec![TableEntry {
            description: "a \"quoted\" name".into(),
            address: AddrExpr::parse("\"m\"+0x10").unwrap(),
            ty: ScanType::U32,
            value: "7".into(),
            frozen: false,
        }],
    };
    save(&table, &p).unwrap();
    assert_eq!(load(&p).unwrap(), table);
    let old = fs::read(&p).unwrap();

    let mut other = table.clone();
    other.entries[0].value = "8".into();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    // Root can write into a read-only directory, so the assertion is moot.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("running as root: skipping read-only directory assertion");
    } else {
        assert!(save(&other, &p).is_err());
        assert_eq!(fs::read(&p).unwrap(), old);
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    save(&other, &p).unwrap();
    assert_eq!(load(&p).unwrap(), other);
    let leftovers = fs::read_dir(&dir).unwrap().count();
    assert_eq!(leftovers, 1, "temp file left behind");
    let _ = fs::remove_dir_all(&dir);
}

fn entry(description: &str, address: AddrExpr) -> TableEntry {
    TableEntry {
        description: description.into(),
        address,
        ty: ScanType::U32,
        value: "1".into(),
        frozen: false,
    }
}

/// `[…["m"+0]+0…]+0` with `levels` dereferences, built without parsing.
fn deep(levels: usize) -> AddrExpr {
    let mut e = Expr::Module("m".into());
    for _ in 0..levels {
        e = Expr::Add(Box::new(Expr::Deref(Box::new(e))), Box::new(Expr::Num(0)));
    }
    AddrExpr(e)
}

#[test]
fn save_refuses_entries_that_would_not_load() {
    let dir = tempdir("roundtrip");
    let p = dir.join("t.toml");
    let quoted = AddrExpr(Expr::Module("we\"ird".into()));
    for (bad, why) in [(quoted, "we\"ird"), (deep(65), "nested")] {
        let table = CheatTable {
            process: None,
            entries: vec![entry("fine", deep(1)), entry("gold", bad)],
        };
        let e = to_toml(&table).expect_err(why);
        assert!(e.message.contains("entry 2"), "{}", e.message);
        assert!(e.message.contains("\"gold\""), "{}", e.message);
        let e = save(&table, &p).expect_err(why);
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
        assert!(e.to_string().contains("\"gold\""), "{e}");
        assert!(!p.exists(), "nothing written");
    }
    let ok = CheatTable {
        process: None,
        entries: vec![entry("fine", deep(64))],
    };
    save(&ok, &p).unwrap();
    assert_eq!(load(&p).unwrap(), ok);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn newer_version_reported_before_unknown_keys() {
    let e = err("version = 2\nfuture = true\n\n[[entry]]\nnew_key = 1\n");
    assert!(e.message.contains("unsupported version 2"), "{}", e.message);
    assert_eq!(e.line, Some(1));
}

#[test]
fn save_does_not_follow_planted_temp_symlink() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir("symlink");
    let p = dir.join("t.toml");
    let victim = dir.join("victim");
    fs::write(&victim, "keep me").unwrap();
    // The old predictable temp name.
    let planted = dir.join(format!("t.toml.tmp-{}", std::process::id()));
    std::os::unix::fs::symlink(&victim, &planted).unwrap();
    let table = CheatTable {
        process: None,
        entries: vec![entry("a", deep(1))],
    };
    save(&table, &p).unwrap();
    assert_eq!(fs::read_to_string(&victim).unwrap(), "keep me");
    let meta = fs::symlink_metadata(&p).unwrap();
    assert!(meta.file_type().is_file(), "saved over a regular file");
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert_eq!(load(&p).unwrap(), table);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn save_replaces_instead_of_rewriting_in_place() {
    let dir = tempdir("hardlink");
    let p = dir.join("t.toml");
    fs::write(&p, "old").unwrap();
    let other = dir.join("other-link");
    fs::hard_link(&p, &other).unwrap();
    let table = CheatTable {
        process: None,
        entries: vec![entry("a", deep(1))],
    };
    save(&table, &p).unwrap();
    assert_eq!(fs::read_to_string(&other).unwrap(), "old");
    assert_eq!(load(&p).unwrap(), table);
    let _ = fs::remove_dir_all(&dir);
}

/// Run `f` on a thread and give up after a few seconds (a FIFO open or read
/// that blocks would otherwise hang the test run).
fn within_secs<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(std::time::Duration::from_secs(5)).ok()
}

#[test]
fn fifo_is_refused_promptly() {
    let dir = tempdir("fifo");
    let p = dir.join("t.toml");
    let c = std::ffi::CString::new(p.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let e = within_secs(move || load(&p))
        .expect("load returned within 5 s")
        .unwrap_err();
    assert!(e.message.contains("not a regular file"), "{}", e.message);
    let _ = fs::remove_dir_all(&dir);
}
