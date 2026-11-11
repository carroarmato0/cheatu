//! Cheat table file format: round trips, refusals, limits, atomic save.

use cheatu_core::table::{
    load, parse, save, to_toml, CheatTable, TableEntry, MAX_ENTRIES, MAX_FILE_BYTES,
};
use cheatu_core::{AddrExpr, ScanType};
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
    let again = parse(&to_toml(&t)).unwrap();
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
    assert_eq!(parse(&to_toml(&t)).unwrap(), t);
    let t = parse(&entry_text("type = \"int\"\nvalue = \"5\"")).unwrap();
    assert_eq!(t.entries[0].ty, ScanType::I32);
    assert!(to_toml(&t).contains("type = \"i32\""));
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
