//! Cheat table and address expression commands: `add`, `entries`, `resolve`,
//! `save`, `load`. They write to a `Write` so they can be tested.

use std::io::{self, Write};
use std::path::Path;

use cheatu_core::table::{self, CheatTable, TableEntry};
use cheatu_core::{AddrExpr, LiveSource, MemSource, ScanType, ScanValue};

/// Split the first expression off `s`. An expression ends at the first
/// whitespace outside quotes, brackets and parentheses, so
/// `["game.exe"+0x10]+8 i32 gold` gives `(["game.exe"+0x10]+8, "i32 gold")`.
pub fn split_expr(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    let mut depth = 0usize;
    let mut quoted = false;
    for (i, c) in s.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '[' | '(' if !quoted => depth += 1,
            ']' | ')' if !quoted => depth = depth.saturating_sub(1),
            c if c.is_whitespace() && !quoted && depth == 0 => {
                return (&s[..i], s[i..].trim_start());
            }
            _ => {}
        }
    }
    (s, "")
}

/// The address in `tok` when it is made only of hex digits (`0x` optional),
/// the legacy form of `write`.
pub fn hex_only(tok: &str) -> Option<Result<u64, String>> {
    let digits = tok
        .strip_prefix("0x")
        .or_else(|| tok.strip_prefix("0X"))
        .unwrap_or(tok);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(u64::from_str_radix(digits, 16).map_err(|_| format!("address {tok} is out of range")))
}

/// Resolve `tok` (hex digits or an expression) to an address. Only for the
/// legacy `write`; everything else takes an expression (decimal digits).
pub fn parse_target(src: &dyn MemSource, tok: &str) -> Result<u64, String> {
    if let Some(addr) = hex_only(tok) {
        return addr;
    }
    let expr = AddrExpr::parse(tok).map_err(|e| format!("bad expression: {e}"))?;
    expr.resolve(src).map_err(|e| e.to_string())
}

fn read_value(src: &dyn MemSource, addr: u64, ty: ScanType) -> Option<ScanValue> {
    let mut buf = vec![0u8; ty.size()];
    match src.read(addr, &mut buf) {
        Ok(n) if n == buf.len() => ScanValue::from_ne_bytes(ty, &buf),
        _ => None,
    }
}

pub fn add(
    out: &mut dyn Write,
    table: &mut Vec<TableEntry>,
    src: Option<&LiveSource>,
    default_ty: ScanType,
    rest: &str,
) -> io::Result<()> {
    let (expr_text, tail) = split_expr(rest);
    if expr_text.is_empty() {
        return writeln!(out, "usage: add <expr> [type] [description…]");
    }
    // Whatever parses reads back the same; checked anyway so the table can
    // never hold an entry `save` refuses.
    let address = match AddrExpr::parse(expr_text).and_then(|a| a.validate().map(|()| a)) {
        Ok(a) => a,
        Err(e) => return writeln!(out, "bad expression: {e}"),
    };
    let (first, after) = tail.split_once(char::is_whitespace).unwrap_or((tail, ""));
    let (ty, description) = match ScanType::from_label(first) {
        Some(t) if !first.is_empty() => (t, after.trim()),
        _ => (default_ty, tail),
    };
    // Start from the current value when it can be read, so the entry is valid.
    let value = src
        .and_then(|s| {
            let addr = address.resolve(s).ok()?;
            read_value(s, addr, ty)
        })
        .map(|v| v.to_string())
        .unwrap_or_else(|| "0".into());
    writeln!(
        out,
        "added entry {}: {address} ({ty}) = {value}",
        table.len()
    )?;
    table.push(TableEntry {
        description: description.to_string(),
        address,
        ty,
        value,
        frozen: false,
    });
    Ok(())
}

pub fn entries(
    out: &mut dyn Write,
    table: &[TableEntry],
    src: Option<&LiveSource>,
) -> io::Result<()> {
    if table.is_empty() {
        return writeln!(out, "(no entries; use `add <expr>` or `load <file>`)");
    }
    for (i, e) in table.iter().enumerate() {
        let desc = if e.description.is_empty() {
            "-"
        } else {
            &e.description
        };
        let state = match src {
            None => "not attached".to_string(),
            Some(s) => match e.address.resolve(s) {
                Err(err) => format!("error: {err}"),
                Ok(addr) => {
                    let value = read_value(s, addr, e.ty)
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "?".into());
                    format!("{addr:#014x} = {value}")
                }
            },
        };
        writeln!(
            out,
            "  [{i:>3}] {desc}  {}  ({})  {state}",
            e.address,
            e.ty.label()
        )?;
    }
    Ok(())
}

pub fn resolve(out: &mut dyn Write, src: Option<&LiveSource>, rest: &str) -> io::Result<()> {
    let Some(src) = src else {
        return writeln!(out, "not attached.");
    };
    if rest.is_empty() {
        return writeln!(out, "usage: resolve <expr>");
    }
    let expr = match AddrExpr::parse(rest) {
        Ok(e) => e,
        Err(e) => return writeln!(out, "bad expression: {e}"),
    };
    match expr.resolve(src) {
        Ok(addr) => writeln!(out, "{expr} = {addr:#x}"),
        Err(e) => writeln!(out, "{expr}: {e}"),
    }
}

pub fn save(
    out: &mut dyn Write,
    table: &[TableEntry],
    process: Option<&str>,
    rest: &str,
) -> io::Result<()> {
    if rest.is_empty() {
        return writeln!(out, "usage: save <file>");
    }
    let t = CheatTable {
        process: process.map(str::to_string),
        entries: table.to_vec(),
    };
    match table::save(&t, Path::new(rest)) {
        Ok(()) => writeln!(out, "saved {} entries to {rest}.", t.entries.len()),
        Err(e) => writeln!(out, "save failed: {e}"),
    }
}

/// Loading never writes memory; every entry comes in unfrozen.
pub fn load(out: &mut dyn Write, table: &mut Vec<TableEntry>, rest: &str) -> io::Result<()> {
    if rest.is_empty() {
        return writeln!(out, "usage: load <file>");
    }
    match table::load(Path::new(rest)) {
        Ok(mut t) => {
            for e in &mut t.entries {
                e.frozen = false;
            }
            writeln!(out, "loaded {} entries, freezes off", t.entries.len())?;
            *table = t.entries;
            Ok(())
        }
        Err(e) => writeln!(out, "load failed: {e}"),
    }
}
