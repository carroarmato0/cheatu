//! cheatu-testbed: a tiny dungeon game to point cheatu at. See `docs/testbed.md`.
//!
//! `cheatu-testbed` plays interactively; `cheatu-testbed --script` speaks the
//! line protocol the integration tests use.

mod script;
mod world;

use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use world::Outcome;

fn status_line() -> String {
    let v = world::values();
    format!(
        "Level {}  HP {}/100  Mana {}  XP {}  Gold {}  Ammo {}  Kills {}",
        v.level, v.hp, v.mana, v.xp, v.gold, v.ammo, v.kills
    )
}

fn interactive() -> io::Result<()> {
    let mut out = io::stdout().lock();
    writeln!(
        out,
        "cheatu-testbed — pid {}. Type help.",
        std::process::id()
    )?;
    writeln!(out, "{}", status_line())?;
    let mut lines = io::stdin().lock().lines();
    loop {
        write!(out, "> ")?;
        out.flush()?;
        let Some(line) = lines.next() else {
            writeln!(out)?;
            return Ok(());
        };
        let line = line?;
        let cmd = line.trim();
        world::refresh_shown();
        match cmd {
            "" => continue,
            "quit" => return Ok(()),
            "help" => writeln!(out, "Commands: {}", script::HELP)?,
            "status" => writeln!(out, "{}", status_line())?,
            "addrs" => writeln!(out, "{}", script::addrs_line())?,
            "marker" => writeln!(out, "{}", world::marker_hex())?,
            _ if cmd.starts_with("get ") || cmd == "get" => {
                match cmd.split_whitespace().nth(1).and_then(script::get) {
                    Some(v) => writeln!(out, "{v}")?,
                    None => writeln!(
                        out,
                        "get what? gold, kills, gold_shown, hp, mana, xp, ammo, level"
                    )?,
                }
            }
            _ => match script::action(cmd).map(world::apply) {
                Some(Outcome::Ok) => writeln!(out, "{}", status_line())?,
                Some(Outcome::Died) => {
                    writeln!(out, "You died! Starting over.")?;
                    writeln!(out, "{}", status_line())?;
                }
                Some(Outcome::Refused(why)) => writeln!(out, "Can't: {why}.")?,
                None => writeln!(out, "Unknown command. Type help.")?,
            },
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let script = match args.as_slice() {
        [] => false,
        [a] if a == "--script" => true,
        _ => {
            eprintln!("usage: cheatu-testbed [--script]");
            return ExitCode::from(2);
        }
    };
    world::init();
    let r = if script {
        script::run(io::stdin().lock(), io::stdout().lock())
    } else {
        interactive()
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cheatu-testbed: {e}");
            ExitCode::FAILURE
        }
    }
}
