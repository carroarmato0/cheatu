//! cheatu-inject — CLI harness for the NW.js JavaScript-injection backend.

use std::path::Path;
use std::thread;
use std::time::Duration;

use serde_json::json;

use cheatu_inject::agent;
use cheatu_inject::cdp::Cdp;
use cheatu_inject::config::{self, DEFAULT_PORT};
use cheatu_inject::{detect_dir, RpgMaker};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let rest = &args[1..];

    let result = match cmd {
        "games" => cmd_games(),
        "detect" => cmd_detect(rest),
        "enable" => cmd_enable(rest),
        "disable" => cmd_disable(rest),
        "info" => cmd_info(rest),
        "eval" => cmd_eval(rest),
        "get-gold" => cmd_get_gold(rest),
        "set-gold" => cmd_set_gold(rest),
        "set-var" => cmd_set_var(rest),
        "freeze-eval" => cmd_freeze_eval(rest),
        // Injected-agent backend (RPG Maker under Proton).
        "install-agent" => cmd_install_agent(rest),
        "uninstall-agent" => cmd_uninstall_agent(rest),
        "agent-state" => cmd_agent_state(rest),
        "agent-get" => cmd_agent_get(rest),
        "agent-set" => cmd_agent_set(rest),
        "agent-set-gold" => cmd_agent_set_gold(rest),
        "agent-freeze" => cmd_agent_freeze(rest),
        "agent-freeze-max" => cmd_agent_freeze_max(rest),
        "agent-unfreeze" => cmd_agent_unfreeze(rest),
        "agent-eval" => cmd_agent_eval(rest),
        "agent-party" => cmd_agent_party(rest),
        "agent-catalog" => cmd_agent_catalog(rest),
        "agent-give" => cmd_agent_give(rest),
        "agent-win" => cmd_simple(rest, agent::battle_win, "forced battle victory"),
        "agent-lose" => cmd_simple(rest, agent::battle_lose, "forced battle defeat"),
        "agent-recover" => cmd_simple(rest, agent::recover, "sent recover"),
        "agent-var" => cmd_agent_var(rest),
        "agent-switch" => cmd_agent_switch(rest),
        "agent-item" => cmd_agent_item(rest),
        _ => {
            usage();
            return;
        }
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn usage() {
    eprintln!(
        "usage:
  cheatu-inject detect <game-dir>
  cheatu-inject enable <game-dir> [port]     # add --remote-debugging-port (backs up package.json)
  cheatu-inject disable <game-dir>           # restore package.json
  cheatu-inject info <port>                  # engine, title, gold
  cheatu-inject eval <port> <js...>          # evaluate JS in the game
  cheatu-inject get-gold <port>
  cheatu-inject set-gold <port> <amount>
  cheatu-inject set-var <port> <id> <value>
  cheatu-inject freeze-eval <port> <js-assignment> [interval-ms]

  --- injected-agent backend (RPG Maker under Proton) ---
  cheatu-inject install-agent <game-dir>     # inject the plugin (restart game after)
  cheatu-inject uninstall-agent <game-dir>   # remove it, restore index.html
  cheatu-inject agent-state <game-dir>       # engine, title, gold, freezes
  cheatu-inject agent-get <game-dir> <js>    # read a JS expression's value
  cheatu-inject agent-set <game-dir> <js-lvalue> <json-value>
  cheatu-inject agent-set-gold <game-dir> <amount>
  cheatu-inject agent-freeze <game-dir> <key> <js-lvalue> <json-value>
  cheatu-inject agent-freeze-max <game-dir> <hp|mp|tp> [all]   # keep stat maxed (level-safe)
  cheatu-inject agent-unfreeze <game-dir> <key>
  cheatu-inject agent-eval <game-dir> <js>   # run arbitrary JS once
  cheatu-inject agent-party <game-dir>       # list party members + stats
  cheatu-inject agent-catalog <game-dir>     # discover items/weapons/armors/variables/switches
  cheatu-inject agent-give <game-dir> <item|weapon|armor> <id> <count>
  cheatu-inject agent-win <game-dir>         # force the current battle to victory
  cheatu-inject agent-lose <game-dir>        # force the current battle to defeat
  cheatu-inject agent-recover <game-dir>     # try to resume after a crash/freeze
  cheatu-inject agent-var <game-dir> get <id> | set <id> <value>
  cheatu-inject agent-switch <game-dir> <id> <on|off>
  cheatu-inject agent-item <game-dir> <itemId> <count>"
    );
}

/// Keep an actor stat pinned to its maximum, re-derived each tick (so it
/// survives level-ups). Uses only standard RPG Maker MV/MZ fields.
fn cmd_agent_freeze_max(args: &[String]) -> Result<(), String> {
    let dir = game_dir(args)?;
    let stat = args.get(1).map(String::as_str).ok_or("need <hp|mp|tp>")?;
    let all = args.get(2).map(String::as_str) == Some("all");
    let assign = match stat {
        "hp" => "a._hp=a.mhp;",
        "mp" => "a._mp=a.mmp;",
        "tp" => "a._tp=a.maxTp();",
        other => return Err(format!("unknown stat {other:?}; use hp, mp, or tp")),
    };
    let run = if all {
        format!("$gameParty.members().forEach(function(a){{{assign}}});")
    } else {
        format!("(function(){{var a=$gameParty.leader();{assign}}})();")
    };
    let key = format!("max_{stat}{}", if all { "_all" } else { "" });
    agent::send(dir, json!([{ "type": "freeze", "key": key, "run": run }]))?;
    println!(
        "freezing {stat} at max ({})",
        if all { "whole party" } else { "leader" }
    );
    Ok(())
}

fn cmd_agent_party(args: &[String]) -> Result<(), String> {
    let dir = game_dir(args)?;
    let s = agent::state(dir)?;
    match s.get("party").and_then(|p| p.as_array()) {
        Some(members) if !members.is_empty() => {
            println!(
                "gold: {}",
                s.get("gold").unwrap_or(&serde_json::Value::Null)
            );
            for m in members {
                println!(
                    "  [{}] {}  Lv{}  HP {}/{}  MP {}/{}  TP {}",
                    m.get("id").unwrap_or(&serde_json::Value::Null),
                    m.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
                    m.get("level").unwrap_or(&serde_json::Value::Null),
                    m.get("hp").unwrap_or(&serde_json::Value::Null),
                    m.get("mhp").unwrap_or(&serde_json::Value::Null),
                    m.get("mp").unwrap_or(&serde_json::Value::Null),
                    m.get("mmp").unwrap_or(&serde_json::Value::Null),
                    m.get("tp").unwrap_or(&serde_json::Value::Null),
                );
            }
        }
        _ => println!(
            "no party data (not an RPG Maker game, or agent not updated — restart the game)"
        ),
    }
    Ok(())
}

fn cmd_simple(
    args: &[String],
    action: fn(&Path) -> Result<(), String>,
    ok: &str,
) -> Result<(), String> {
    let dir = game_dir(args)?;
    action(dir)?;
    println!("{ok}");
    Ok(())
}

fn cmd_agent_catalog(args: &[String]) -> Result<(), String> {
    let dir = game_dir(args)?;
    agent::request_catalog(dir)?;
    std::thread::sleep(std::time::Duration::from_millis(500));
    let c = agent::read_catalog(dir).ok_or("no catalog (game running with the updated agent?)")?;
    println!(
        "items: {}  weapons: {}  armors: {}  variables: {}  switches: {}",
        c.items.len(),
        c.weapons.len(),
        c.armors.len(),
        c.variables.len(),
        c.switches.len()
    );
    for e in c.items.iter().take(8) {
        println!("  item   {:>4}  {}  (have {})", e.id, e.name, e.count);
    }
    for e in c.variables.iter().take(8) {
        println!("  var    {:>4}  {} = {}", e.id, e.name, e.value);
    }
    for e in c.switches.iter().take(8) {
        println!("  switch {:>4}  {} = {}", e.id, e.name, e.value);
    }
    Ok(())
}

fn cmd_agent_give(args: &[String]) -> Result<(), String> {
    let dir = game_dir(args)?;
    let kind = match args.get(1).map(String::as_str) {
        Some("item") => agent::ItemKind::Item,
        Some("weapon") => agent::ItemKind::Weapon,
        Some("armor") => agent::ItemKind::Armor,
        _ => return Err("usage: agent-give <dir> <item|weapon|armor> <id> <count>".into()),
    };
    let id: i64 = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .ok_or("need <id>")?;
    let count: i64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
    agent::gain_item(dir, kind, id, count)?;
    println!("gave {kind:?} {id} x{count}");
    Ok(())
}

fn cmd_agent_var(args: &[String]) -> Result<(), String> {
    let dir = game_dir(args)?;
    match args.get(1).map(String::as_str) {
        Some("get") => {
            let id: u32 = args
                .get(2)
                .and_then(|s| s.parse().ok())
                .ok_or("need <id>")?;
            cmd_agent_get(&[
                dir.to_string_lossy().into_owned(),
                format!("$gameVariables.value({id})"),
            ])
        }
        Some("set") => {
            let id: u32 = args
                .get(2)
                .and_then(|s| s.parse().ok())
                .ok_or("need <id>")?;
            let value = parse_json_value(args.get(3).ok_or("need <value>")?);
            agent::send(
                dir,
                json!([{ "type": "eval", "expr": format!("$gameVariables.setValue({id}, {value})") }]),
            )?;
            println!("set variable {id} = {value}");
            Ok(())
        }
        _ => Err("usage: agent-var <dir> get <id> | set <id> <value>".into()),
    }
}

fn cmd_agent_switch(args: &[String]) -> Result<(), String> {
    let dir = game_dir(args)?;
    let id: u32 = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .ok_or("need <id>")?;
    let on = matches!(args.get(2).map(String::as_str), Some("on" | "true" | "1"));
    agent::send(
        dir,
        json!([{ "type": "eval", "expr": format!("$gameSwitches.setValue({id}, {on})") }]),
    )?;
    println!("set switch {id} = {on}");
    Ok(())
}

fn cmd_agent_item(args: &[String]) -> Result<(), String> {
    let dir = game_dir(args)?;
    let item_id: u32 = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .ok_or("need <itemId>")?;
    let count: i64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    agent::send(
        dir,
        json!([{ "type": "eval", "expr": format!("$gameParty.gainItem($dataItems[{item_id}], {count})") }]),
    )?;
    println!("gave item {item_id} x{count}");
    Ok(())
}

fn game_dir(args: &[String]) -> Result<&Path, String> {
    args.first().map(Path::new).ok_or("need <game-dir>".into())
}

fn cmd_install_agent(args: &[String]) -> Result<(), String> {
    let dir = game_dir(args)?;
    let prior = agent::installed_hash(dir);
    agent::install(dir)?;
    let now = agent::agent_hash();
    let short = &now[..8.min(now.len())];
    match prior {
        Some(h) if h == now => {
            println!("agent already current ({short}). Restart the game if it was running.")
        }
        Some(_) => println!("agent updated ({short}). Restart the game to load it."),
        None => println!(
            "agent installed ({short}). Restart the game (normal launch — no debug flags needed)."
        ),
    }
    Ok(())
}

fn cmd_uninstall_agent(args: &[String]) -> Result<(), String> {
    let dir = game_dir(args)?;
    agent::uninstall(dir)?;
    println!("agent removed; index.html restored.");
    Ok(())
}

fn cmd_agent_state(args: &[String]) -> Result<(), String> {
    let dir = game_dir(args)?;
    let s = agent::state(dir)?;
    println!("{}", serde_json::to_string_pretty(&s).unwrap_or_default());
    Ok(())
}

/// Set a watch, wait for the agent to report it, and print the value.
fn cmd_agent_get(args: &[String]) -> Result<(), String> {
    let dir = game_dir(args)?;
    let expr = args.get(1).ok_or("need <js>")?.clone();
    agent::send(dir, json!([{ "type": "watch", "exprs": [expr] }]))?;
    std::thread::sleep(std::time::Duration::from_millis(400));
    let s = agent::state(dir)?;
    println!(
        "{} = {}",
        expr,
        s.get("watch")
            .and_then(|w| w.get(&expr))
            .unwrap_or(&serde_json::Value::Null)
    );
    Ok(())
}

fn parse_json_value(s: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap_or_else(|_| serde_json::Value::String(s.to_string()))
}

fn cmd_agent_set(args: &[String]) -> Result<(), String> {
    let dir = game_dir(args)?;
    let expr = args.get(1).ok_or("need <js-lvalue>")?;
    let value = parse_json_value(args.get(2).ok_or("need <json-value>")?);
    agent::send(
        dir,
        json!([{ "type": "set", "expr": expr, "value": value }]),
    )?;
    println!("set {expr} = {value}");
    Ok(())
}

fn cmd_agent_set_gold(args: &[String]) -> Result<(), String> {
    let dir = game_dir(args)?;
    let amount: i64 = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .ok_or("need <amount>")?;
    agent::send(
        dir,
        json!([{ "type": "set", "expr": "$gameParty._gold", "value": amount }]),
    )?;
    println!("set gold to {amount}");
    Ok(())
}

fn cmd_agent_freeze(args: &[String]) -> Result<(), String> {
    let dir = game_dir(args)?;
    let key = args.get(1).ok_or("need <key>")?;
    let expr = args.get(2).ok_or("need <js-lvalue>")?;
    let value = parse_json_value(args.get(3).ok_or("need <json-value>")?);
    agent::send(
        dir,
        json!([{ "type": "freeze", "key": key, "expr": expr, "value": value }]),
    )?;
    println!("freezing {key}: {expr} = {value}");
    Ok(())
}

fn cmd_agent_unfreeze(args: &[String]) -> Result<(), String> {
    let dir = game_dir(args)?;
    let key = args.get(1).ok_or("need <key>")?;
    agent::send(dir, json!([{ "type": "unfreeze", "key": key }]))?;
    println!("unfroze {key}");
    Ok(())
}

fn cmd_agent_eval(args: &[String]) -> Result<(), String> {
    let dir = game_dir(args)?;
    let expr = args[1..].join(" ");
    if expr.is_empty() {
        return Err("need <js>".into());
    }
    agent::send(dir, json!([{ "type": "eval", "expr": expr }]))?;
    println!("sent eval: {expr}");
    Ok(())
}

fn cmd_games() -> Result<(), String> {
    let games = cheatu_inject::find_rpgmaker_games();
    if games.is_empty() {
        println!("no NW.js games found in the usual Steam folders.");
    }
    for g in games {
        println!(
            "{:<14} {}  ->  {}",
            g.engine.label(),
            g.name,
            g.path.display()
        );
    }
    Ok(())
}

fn cmd_detect(args: &[String]) -> Result<(), String> {
    let dir = args.first().ok_or("need <game-dir>")?;
    let engine = detect_dir(Path::new(dir));
    println!("engine: {}", engine.label());
    println!(
        "nwjs: {}   rpgmaker: {}",
        engine.is_nwjs(),
        engine.is_rpgmaker()
    );
    Ok(())
}

fn cmd_enable(args: &[String]) -> Result<(), String> {
    let dir = args.first().ok_or("need <game-dir>")?;
    let port: u16 = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let changed = config::enable_remote_debugging(Path::new(dir), port)?;
    if changed {
        println!("enabled --remote-debugging-port={port} (backup: package.json.cheatu-bak).");
        println!("restart the game for it to take effect.");
    } else {
        println!("remote debugging already enabled.");
    }
    Ok(())
}

fn cmd_disable(args: &[String]) -> Result<(), String> {
    let dir = args.first().ok_or("need <game-dir>")?;
    if config::disable_remote_debugging(Path::new(dir))? {
        println!("restored original package.json.");
    } else {
        println!("no backup found; nothing to restore.");
    }
    Ok(())
}

fn connect(args: &[String]) -> Result<(Cdp, u16), String> {
    let port: u16 = args
        .first()
        .and_then(|s| s.parse().ok())
        .ok_or("need <port>")?;
    Ok((Cdp::connect(port)?, port))
}

fn cmd_info(args: &[String]) -> Result<(), String> {
    let (mut cdp, port) = connect(args)?;
    let mut rm = RpgMaker::new(&mut cdp);
    println!("connected on port {port}");
    if rm.is_present() {
        println!("engine: RPG Maker {}", rm.engine_name());
        println!("title:  {}", rm.game_title());
        match rm.gold() {
            Ok(g) => println!("gold:   {g}"),
            Err(e) => println!("gold:   <unavailable: {e}>"),
        }
        if let Ok(names) = rm.party_members() {
            println!("party:  {}", names.join(", "));
        }
    } else {
        println!("RPG Maker runtime not detected (title screen, or non-RPG-Maker NW.js app).");
    }
    Ok(())
}

fn cmd_eval(args: &[String]) -> Result<(), String> {
    let (mut cdp, _) = connect(args)?;
    let expr = args[1..].join(" ");
    if expr.is_empty() {
        return Err("need <js...>".into());
    }
    println!("{}", cdp.eval(&expr)?);
    Ok(())
}

fn cmd_get_gold(args: &[String]) -> Result<(), String> {
    let (mut cdp, _) = connect(args)?;
    println!("{}", RpgMaker::new(&mut cdp).gold()?);
    Ok(())
}

fn cmd_set_gold(args: &[String]) -> Result<(), String> {
    let (mut cdp, _) = connect(args)?;
    let amount: i64 = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .ok_or("need <amount>")?;
    RpgMaker::new(&mut cdp).set_gold(amount)?;
    println!("set gold to {amount}");
    Ok(())
}

fn cmd_set_var(args: &[String]) -> Result<(), String> {
    let (mut cdp, _) = connect(args)?;
    let id: u32 = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .ok_or("need <id>")?;
    let value: f64 = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .ok_or("need <value>")?;
    RpgMaker::new(&mut cdp).set_variable(id, value)?;
    println!("set variable {id} = {value}");
    Ok(())
}

/// Repeatedly evaluate a JS assignment to hold a value (stable JS-side freeze).
fn cmd_freeze_eval(args: &[String]) -> Result<(), String> {
    let (mut cdp, _) = connect(args)?;
    let expr = args.get(1).ok_or("need <js-assignment>")?.clone();
    let interval: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(500);
    println!("freezing via `{expr}` every {interval}ms (Ctrl-C to stop)…");
    loop {
        if let Err(e) = cdp.eval(&expr) {
            eprintln!("eval failed (game closed?): {e}");
            break;
        }
        thread::sleep(Duration::from_millis(interval));
    }
    Ok(())
}
