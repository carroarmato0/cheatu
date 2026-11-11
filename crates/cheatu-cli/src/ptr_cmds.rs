//! Pointer scan commands: `ptrscan <target> [depth] [max-offset]` and its
//! `filter`, `save`, `load` and `list` subcommands.

use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use cheatu_core::pointer_scan::{find_chains, PointerMap, PtrResults, PtrScanSettings};
use cheatu_core::{AddrExpr, LiveSource, MemSource, ScanProgress, ScanType};

use crate::table_cmds::split_expr;

const NO_RESULTS: &str =
    "no pointer scan results; run `ptrscan <target>` or `ptrscan load <file>`.";

/// Everything a `ptrscan` command may touch.
pub struct PtrCtx<'a> {
    pub src: Option<&'a mut LiveSource>,
    pub process: &'a str,
    pub ty: ScanType,
}

pub fn run(
    out: &mut dyn Write,
    ptr: &mut Option<PtrResults>,
    ctx: PtrCtx,
    rest: &str,
) -> io::Result<()> {
    let (word, tail) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let tail = tail.trim();
    match word {
        "list" => list(out, ptr, tail),
        "save" => save(out, ptr, tail),
        "load" => load(out, ptr, tail),
        "filter" => filter(out, ptr, ctx, tail),
        _ => scan(out, ptr, ctx, rest),
    }
}

/// Resolve a target address expression. Unlike the legacy `write`, bare
/// digits are decimal: hex needs `0x`.
fn resolve_target(src: &dyn MemSource, tok: &str) -> Result<u64, String> {
    let expr = AddrExpr::parse(tok).map_err(|e| format!("bad expression: {e}"))?;
    expr.resolve(src).map_err(|e| e.to_string())
}

fn parse_num(s: &str) -> Option<u64> {
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16).ok(),
        None => s.parse().ok(),
    }
}

/// Run `work` on a thread while printing its progress on one line.
fn with_progress<T: Send>(
    out: &mut dyn Write,
    label: &str,
    progress: &ScanProgress,
    work: impl FnOnce() -> T + Send,
) -> T {
    std::thread::scope(|s| {
        let handle = s.spawn(work);
        let mut last = None;
        while !handle.is_finished() {
            if let Some(f) = progress.fraction() {
                let pct = (f * 100.0) as u32;
                if last != Some(pct) {
                    last = Some(pct);
                    let _ = write!(out, "\r{label}: {pct:>3}%");
                    let _ = out.flush();
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if last.is_some() {
            let _ = writeln!(out);
        }
        handle.join().expect("pointer scan thread panicked")
    })
}

fn scan(
    out: &mut dyn Write,
    ptr: &mut Option<PtrResults>,
    ctx: PtrCtx,
    rest: &str,
) -> io::Result<()> {
    let Some(src) = ctx.src else {
        return writeln!(out, "not attached.");
    };
    let (target_tok, args) = split_expr(rest);
    if target_tok.is_empty() {
        return writeln!(
            out,
            "usage: ptrscan <expr> [depth] [max-offset] | filter addr <expr> | \
             filter value <v> | save <file> | load <file> | list [n]"
        );
    }
    // Defaults only: a loaded results file never supplies settings.
    let mut settings = PtrScanSettings::default();
    let mut args = args.split_whitespace();
    if let Some(d) = args.next() {
        match d.parse::<usize>() {
            Ok(d) if (1..=8).contains(&d) => settings.max_depth = d,
            _ => return writeln!(out, "depth must be a number from 1 to 8."),
        }
    }
    if let Some(m) = args.next() {
        match parse_num(m) {
            Some(m) if (0x8..=0x100000).contains(&m) => settings.max_offset = m,
            _ => return writeln!(out, "max-offset must be from 0x8 to 0x100000."),
        }
    }
    if args.next().is_some() {
        return writeln!(out, "usage: ptrscan <expr> [depth] [max-offset]");
    }

    if let Err(e) = src.refresh_regions() {
        return writeln!(out, "could not read the memory map: {e}");
    }
    let src: &LiveSource = src;
    let target = match resolve_target(src, target_tok) {
        Ok(t) => t,
        Err(e) => return writeln!(out, "{e}"),
    };
    writeln!(
        out,
        "pointer scan for {target:#x} (depth {}, max offset {:#x})…",
        settings.max_depth, settings.max_offset
    )?;

    let progress = ScanProgress::default();
    let map = with_progress(out, "building pointer map", &progress, || {
        PointerMap::build(src, &settings, &progress)
    });
    let map = match map {
        Ok(m) => m,
        Err(e) => return writeln!(out, "pointer scan failed: {e}"),
    };
    let progress = ScanProgress::default();
    let found = with_progress(out, "searching chains", &progress, || {
        find_chains(&map, src, target, &settings, &progress)
    });
    match found {
        Ok(found) => {
            writeln!(
                out,
                "{} chains found{}.",
                found.chains.len(),
                if found.truncated {
                    " (truncated: more exist)"
                } else {
                    ""
                }
            )?;
            *ptr = Some(PtrResults {
                process: ctx.process.to_string(),
                target,
                settings,
                chains: found.chains,
                truncated: found.truncated,
            });
            Ok(())
        }
        Err(e) => writeln!(out, "pointer scan failed: {e}"),
    }
}

fn filter(
    out: &mut dyn Write,
    ptr: &mut Option<PtrResults>,
    ctx: PtrCtx,
    rest: &str,
) -> io::Result<()> {
    let Some(src) = ctx.src else {
        return writeln!(out, "not attached.");
    };
    let Some(res) = ptr else {
        return writeln!(out, "{NO_RESULTS}");
    };
    let (kind, arg) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let arg = arg.trim();
    if let Err(e) = src.refresh_regions() {
        return writeln!(out, "could not read the memory map: {e}");
    }
    let src: &LiveSource = src;
    let remaining = match kind {
        "addr" if !arg.is_empty() => match resolve_target(src, arg) {
            Ok(a) => res.filter_by_address(src, a),
            Err(e) => return writeln!(out, "{e}"),
        },
        "value" if !arg.is_empty() => match ctx.ty.parse(arg) {
            Some(v) => res.filter_by_value(src, ctx.ty, &v),
            None => return writeln!(out, "could not parse {arg:?} as {}.", ctx.ty),
        },
        _ => return writeln!(out, "usage: ptrscan filter addr <expr> | filter value <v>"),
    };
    writeln!(out, "{remaining} chains remain.")
}

fn save(out: &mut dyn Write, ptr: &Option<PtrResults>, rest: &str) -> io::Result<()> {
    let Some(res) = ptr else {
        return writeln!(out, "{NO_RESULTS}");
    };
    if rest.is_empty() {
        return writeln!(out, "usage: ptrscan save <file>");
    }
    match res.save(Path::new(rest)) {
        Ok(()) => writeln!(out, "saved {} chains to {rest}.", res.chains.len()),
        Err(e) => writeln!(out, "save failed: {e}"),
    }
}

fn load(out: &mut dyn Write, ptr: &mut Option<PtrResults>, rest: &str) -> io::Result<()> {
    if rest.is_empty() {
        return writeln!(out, "usage: ptrscan load <file>");
    }
    match PtrResults::load(Path::new(rest)) {
        Ok(res) => {
            writeln!(
                out,
                "loaded {} chains for {:?} (target was {:#x}).",
                res.chains.len(),
                res.process,
                res.target
            )?;
            *ptr = Some(res);
            Ok(())
        }
        Err(e) => writeln!(out, "load failed: {e}"),
    }
}

fn list(out: &mut dyn Write, ptr: &Option<PtrResults>, rest: &str) -> io::Result<()> {
    let Some(res) = ptr else {
        return writeln!(out, "{NO_RESULTS}");
    };
    let limit: usize = if rest.is_empty() {
        20
    } else {
        match rest.parse() {
            Ok(n) => n,
            Err(_) => return writeln!(out, "usage: ptrscan list [n]"),
        }
    };
    let total = res.chains.len();
    writeln!(
        out,
        "{total} chains to {:#x}{} (showing up to {limit}):",
        res.target,
        if res.truncated { ", truncated" } else { "" }
    )?;
    for (i, c) in res.chains.iter().take(limit).enumerate() {
        writeln!(out, "  [{i:>4}] {}", c.to_expr())?;
    }
    if total > limit {
        writeln!(out, "  … {} more (use `ptrscan list <n>`).", total - limit)?;
    }
    Ok(())
}
