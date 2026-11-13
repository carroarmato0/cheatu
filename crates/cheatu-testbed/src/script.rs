//! Commands shared by both modes, and the `--script` protocol: one reply line
//! per input line, `OK …` or `ERR <reason>`, flushed each time.

use std::io::{self, BufRead, Write};

use crate::world::{self, Action, Outcome, Values};

pub const HELP: &str = "status fight heal shoot buy next addrs get marker help quit";

pub fn action(cmd: &str) -> Option<Action> {
    Some(match cmd {
        "fight" => Action::Fight,
        "heal" => Action::Heal,
        "shoot" => Action::Shoot,
        "buy" => Action::Buy,
        "next" => Action::Next,
        _ => return None,
    })
}

pub fn values_line(v: &Values) -> String {
    format!(
        "gold={} kills={} gold_shown={} hp={} mana={} xp={} ammo={} level={}",
        v.gold, v.kills, v.gold_shown, v.hp, v.mana, v.xp, v.ammo, v.level
    )
}

pub fn addrs_line() -> String {
    let parts: Vec<String> = world::addrs()
        .into_iter()
        .map(|(n, a)| format!("{n}=0x{a:x}"))
        .collect();
    parts.join(" ")
}

pub fn get(name: &str) -> Option<String> {
    let v = world::values();
    Some(match name {
        "gold" => v.gold.to_string(),
        "kills" => v.kills.to_string(),
        "gold_shown" => v.gold_shown.to_string(),
        "hp" => v.hp.to_string(),
        "mana" => v.mana.to_string(),
        "xp" => v.xp.to_string(),
        "ammo" => v.ammo.to_string(),
        "level" => v.level.to_string(),
        _ => return None,
    })
}

/// The reply to one command line, and whether to stop afterwards.
fn reply(line: &str) -> (String, bool) {
    world::refresh_shown();
    let mut words = line.split_whitespace();
    let Some(cmd) = words.next() else {
        return ("ERR empty".into(), false);
    };
    let r = match cmd {
        "status" => format!("OK {}", values_line(&world::values())),
        "addrs" => format!("OK {}", addrs_line()),
        "marker" => format!("OK {}", world::marker_hex()),
        "help" => format!("OK {HELP}"),
        "quit" => return ("OK bye".into(), true),
        "get" => match words.next().and_then(get) {
            Some(v) => format!("OK {v}"),
            None => "ERR unknown value".into(),
        },
        _ => match action(cmd).map(world::apply) {
            Some(Outcome::Ok) => format!("OK {}", values_line(&world::values())),
            Some(Outcome::Died) => format!("OK died {}", values_line(&world::values())),
            Some(Outcome::Refused(why)) => format!("ERR {why}"),
            None => "ERR unknown command".into(),
        },
    };
    (r, false)
}

/// Run the protocol until `quit` or end of input.
pub fn run(input: impl BufRead, mut out: impl Write) -> io::Result<()> {
    writeln!(out, "READY pid={}", std::process::id())?;
    out.flush()?;
    for line in input.lines() {
        let (r, stop) = reply(line?.trim());
        writeln!(out, "{r}")?;
        out.flush()?;
        if stop {
            break;
        }
    }
    Ok(())
}
