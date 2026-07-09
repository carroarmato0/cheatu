//! In-game agent: a JavaScript plugin injected into an RPG Maker MV/MZ game,
//! plus the host-side file bridge cheatu uses to drive it.
//!
//! Why this instead of external CDP: Chromium's remote-debugging server does
//! not start under Wine/Proton, so an external debugger can't attach. But the
//! game's own NW.js runtime has Node `fs`, and the Wine `Z:` drive maps to the
//! Linux root — so the agent and cheatu can exchange JSON through a plain file
//! in the game folder. The agent applies changes via the game's own data model
//! (`$gameParty`, `$gameActors`, …), which is stable (no GC fighting).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

/// The agent's identity is a checksum of its source, so any change to the
/// plugin is detected automatically — no manual version bump needed. cheatu
/// compares this against the installed file and the running copy.
pub fn agent_hash() -> String {
    // FNV-1a over the template: deterministic, dependency-free, content-only.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in AGENT_JS_TEMPLATE.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The plugin source dropped into the game. `__CHEATU_VERSION__` is filled in
/// by [`current_agent_js`]. Polls `cheatu/cmd.json` for actions, re-applies
/// frozen values every tick, and writes live values to `cheatu/state.json`.
const AGENT_JS_TEMPLATE: &str = r#"/*:
 * @plugindesc cheatu agent - file-IPC bridge for external control by cheatu.
 * cheatu-agent-hash: __CHEATU_HASH__
 */
(function () {
  try {
    var AGENT_HASH = "__CHEATU_HASH__";
    var fs = require('fs');
    var path = require('path');
    var base;
    try { base = path.dirname(process.mainModule.filename); }
    catch (e) { base = path.dirname(decodeURIComponent(window.location.pathname)); }
    var dir = path.join(base, 'cheatu');
    try { fs.mkdirSync(dir); } catch (e) {}
    var cmdFile = path.join(dir, 'cmd.json');
    var stateFile = path.join(dir, 'state.json');
    var queryFile = path.join(dir, 'query.json');
    var freezes = {};
    var lastCmd = 0;
    window.__cheatuWatch = window.__cheatuWatch || [];

    function apply(expr, value) { eval(expr + ' = ' + JSON.stringify(value)); }

    function writeQuery(o) { try { fs.writeFileSync(queryFile, JSON.stringify(o)); } catch (e) {} }

    // Try to resume the game after a JavaScript exception froze it — clear the
    // stopped/error state and kick the update loop again. Our setInterval keeps
    // running even when the game's own render loop has died, so this can undo a
    // crash that would otherwise force a relaunch.
    function removeErrorOverlay() {
      // RPG Maker's error display both dims/blurs the game canvas (a CSS filter
      // with no built-in undo) and shows an error <p>. Clear both — the canvas
      // filter is the "transparent overlay" that lingers after a recover.
      try {
        if (typeof Graphics !== 'undefined') {
          if (Graphics._canvas && Graphics._canvas.style) {
            Graphics._canvas.style.opacity = 1;
            Graphics._canvas.style.filter = '';
            Graphics._canvas.style.webkitFilter = '';
          }
          try { Graphics._errorShowed = false; } catch (e) {}
          if (Graphics._errorPrinter && Graphics._errorPrinter.style) {
            Graphics._errorPrinter.style.display = 'none';
          }
        }
      } catch (e) {}
      try {
        ['ErrorPrinter', 'errorPrinter'].forEach(function (id) {
          var el = document.getElementById(id);
          if (el && el.style) el.style.display = 'none';
        });
      } catch (e) {}
    }

    function recover() {
      if (typeof SceneManager === 'undefined') return;
      SceneManager._stopped = false;
      try { SceneManager._error = null; } catch (e) {}
      removeErrorOverlay();
      if (typeof SceneManager.requestUpdate === 'function') {
        SceneManager.requestUpdate();
      } else if (typeof requestAnimationFrame === 'function') {
        requestAnimationFrame(function () { try { SceneManager.update(); } catch (e) {} });
      }
    }

    // Enumerate a database array ($dataItems/$dataWeapons/$dataArmors) into
    // {id, name, count-owned}. Standard MV/MZ layout: index 0 is null.
    function collectDb(db) {
      var r = [];
      if (!db) return r;
      for (var i = 1; i < db.length; i++) {
        var d = db[i];
        if (d && d.name) {
          var count = 0;
          try { count = $gameParty.numItems(d); } catch (e) {}
          r.push({ id: d.id, name: d.name, count: count });
        }
      }
      return r;
    }

    // Enumerate named variables/switches ($dataSystem.variables / .switches),
    // skipping the unnamed (unused) slots.
    function collectNamed(names, getVal) {
      var r = [];
      if (!names) return r;
      for (var i = 1; i < names.length; i++) {
        if (names[i]) {
          var v = null;
          try { v = getVal(i); } catch (e) {}
          r.push({ id: i, name: names[i], value: v });
        }
      }
      return r;
    }

    function buildCatalog() {
      var sys = (typeof $dataSystem !== 'undefined' && $dataSystem) || {};
      return {
        items: collectDb(typeof $dataItems !== 'undefined' ? $dataItems : null),
        weapons: collectDb(typeof $dataWeapons !== 'undefined' ? $dataWeapons : null),
        armors: collectDb(typeof $dataArmors !== 'undefined' ? $dataArmors : null),
        variables: collectNamed(sys.variables, function (i) { return $gameVariables.value(i); }),
        switches: collectNamed(sys.switches, function (i) { return $gameSwitches.value(i); })
      };
    }

    function handle(cmd) {
      if (!cmd || !cmd.actions) return;
      cmd.actions.forEach(function (a) {
        try {
          switch (a.type) {
            case 'set': apply(a.expr, a.value); break;
            case 'eval': eval(a.expr); break;
            // A freeze either sets expr=value each tick, or runs a statement
            // (a.run) each tick — the latter allows "keep X at its max".
            case 'freeze': freezes[a.key] = a.run ? { run: a.run } : { expr: a.expr, value: a.value }; break;
            case 'unfreeze': delete freezes[a.key]; break;
            case 'unfreeze_all': freezes = {}; break;
            case 'watch': window.__cheatuWatch = a.exprs || []; break;
            case 'catalog': writeQuery({ key: 'catalog', ts: Date.now(), value: buildCatalog() }); break;
            case 'query': writeQuery({ key: a.key || 'query', ts: Date.now(), value: eval(a.expr) }); break;
            case 'battle_win': if (typeof BattleManager !== 'undefined' && $gameParty.inBattle()) BattleManager.processVictory(); break;
            case 'battle_lose': if (typeof BattleManager !== 'undefined' && $gameParty.inBattle()) BattleManager.processDefeat(); break;
            case 'recover': recover(); break;
          }
        } catch (e) {}
      });
    }

    function readCmd() {
      try {
        var st = fs.statSync(cmdFile);
        if (st.mtimeMs === lastCmd) return;
        lastCmd = st.mtimeMs;
        handle(JSON.parse(fs.readFileSync(cmdFile, 'utf8')));
      } catch (e) {}
    }

    function applyFreezes() {
      for (var k in freezes) {
        try {
          var f = freezes[k];
          if (f.run) { eval(f.run); } else { apply(f.expr, f.value); }
        } catch (e) {}
      }
    }

    // A generic snapshot that adapts to any RPG Maker MV/MZ game: gold and
    // every party member's standard stats. No game-specific fields.
    function snapshot() {
      var s = { ts: Date.now(), agentHash: AGENT_HASH };
      try { s.rpgmaker = (typeof $gameParty !== 'undefined' && $gameParty !== null); } catch (e) { s.rpgmaker = false; }
      try { s.engine = (typeof Utils !== 'undefined' && Utils.RPGMAKER_NAME) || ''; } catch (e) {}
      try { s.title = (typeof $dataSystem !== 'undefined' && $dataSystem && $dataSystem.gameTitle) || ''; } catch (e) {}
      try { s.gold = s.rpgmaker ? $gameParty.gold() : null; } catch (e) { s.gold = null; }
      try { s.inBattle = s.rpgmaker && $gameParty.inBattle(); } catch (e) { s.inBattle = false; }
      try { s.stopped = (typeof SceneManager !== 'undefined' && SceneManager._stopped === true); } catch (e) { s.stopped = false; }
      try {
        if (s.rpgmaker) {
          // Prefer raw fields (_hp/_mp/_tp) over getters so we don't run the
          // game's custom stat code from our timer; max HP/MP still need the
          // getter, but this snapshot only runs a few times a second.
          s.party = $gameParty.members().map(function (a) {
            return {
              id: a._actorId, name: a._name, level: a._level,
              hp: a._hp, mhp: a.mhp, mp: a._mp, mmp: a.mmp, tp: a._tp
            };
          });
        }
      } catch (e) {}
      s.watch = {};
      window.__cheatuWatch.forEach(function (w) { try { s.watch[w] = eval(w); } catch (e) { s.watch[w] = null; } });
      s.freezes = Object.keys(freezes);
      return s;
    }

    function writeState() {
      try { fs.writeFileSync(stateFile, JSON.stringify(snapshot())); } catch (e) {}
    }

    // Commands/freezes need to be responsive, but the read-only snapshot polls
    // the game's own getters, which in heavily-modified games can be costly or
    // have side effects — so write it less often to stay out of the game's way.
    var __tick = 0;
    setInterval(function () {
      readCmd();
      applyFreezes();
      if ((__tick++ % 3) === 0) writeState();
    }, 100);
    if (window.console) console.log('[cheatu] agent loaded; ipc dir: ' + dir);
  } catch (e) {
    if (window.console) console.error('[cheatu] agent failed: ' + e);
  }
})();
"#;

/// The agent source for the current build, with its checksum baked in.
pub fn current_agent_js() -> String {
    AGENT_JS_TEMPLATE.replace("__CHEATU_HASH__", &agent_hash())
}

/// The checksum of the agent currently written to the game's plugin file, if
/// installed. Parses the `cheatu-agent-hash:` marker.
pub fn installed_hash(game_dir: &Path) -> Option<String> {
    let text = fs::read_to_string(web_root(game_dir).join(AGENT_REL)).ok()?;
    let marker = "cheatu-agent-hash:";
    let idx = text.find(marker)?;
    text[idx + marker.len()..]
        .split(|c: char| !c.is_ascii_hexdigit())
        .find(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Whether the installed plugin differs from the current build.
pub fn needs_update(game_dir: &Path) -> bool {
    is_installed(game_dir) && installed_hash(game_dir).as_deref() != Some(agent_hash().as_str())
}

const AGENT_REL: &str = "js/plugins/cheatu_agent.js";
const SCRIPT_TAG: &str =
    "<script type=\"text/javascript\" src=\"js/plugins/cheatu_agent.js\"></script>";

/// The `www` directory (or the game dir itself if there's no `www`).
fn web_root(game_dir: &Path) -> PathBuf {
    let www = game_dir.join("www");
    if www.join("index.html").exists() {
        www
    } else {
        game_dir.to_path_buf()
    }
}

fn ipc_dir(game_dir: &Path) -> PathBuf {
    web_root(game_dir).join("cheatu")
}

/// Install the agent: write the plugin and inject a `<script>` tag into
/// `index.html` (backed up first). Idempotent. The game must be restarted for
/// the plugin to load.
pub fn install(game_dir: &Path) -> Result<(), String> {
    let www = web_root(game_dir);
    let plugin_path = www.join(AGENT_REL);
    fs::create_dir_all(plugin_path.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::write(&plugin_path, current_agent_js()).map_err(|e| format!("write agent: {e}"))?;
    fs::create_dir_all(ipc_dir(game_dir)).map_err(|e| e.to_string())?;

    let index = www.join("index.html");
    let mut html = fs::read_to_string(&index).map_err(|e| format!("read index.html: {e}"))?;
    if html.contains("cheatu_agent.js") {
        return Ok(()); // already injected
    }

    let backup = index.with_extension("html.cheatu-bak");
    if !backup.exists() {
        fs::copy(&index, &backup).map_err(|e| format!("backup index.html: {e}"))?;
    }

    // Insert our tag just before the game's main.js script element.
    let anchor = html
        .find("src=\"js/main.js\"")
        .ok_or("could not find js/main.js in index.html")?;
    let script_start = html[..anchor]
        .rfind("<script")
        .ok_or("malformed index.html")?;
    html.insert_str(script_start, &format!("{SCRIPT_TAG}\n    "));
    fs::write(&index, html).map_err(|e| format!("write index.html: {e}"))?;
    Ok(())
}

/// Remove the agent and restore `index.html`.
pub fn uninstall(game_dir: &Path) -> Result<(), String> {
    let www = web_root(game_dir);
    let index = www.join("index.html");
    let backup = index.with_extension("html.cheatu-bak");
    if backup.exists() {
        fs::copy(&backup, &index).map_err(|e| format!("restore index.html: {e}"))?;
        fs::remove_file(&backup).ok();
    }
    fs::remove_file(www.join(AGENT_REL)).ok();
    Ok(())
}

/// Whether the agent plugin is installed.
pub fn is_installed(game_dir: &Path) -> bool {
    web_root(game_dir).join(AGENT_REL).exists()
}

/// Send a batch of actions to the running agent (writes `cmd.json`).
pub fn send(game_dir: &Path, actions: Value) -> Result<(), String> {
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let payload = json!({ "id": id, "actions": actions });
    fs::write(
        ipc_dir(game_dir).join("cmd.json"),
        serde_json::to_string(&payload).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write cmd.json: {e}"))
}

/// Read the agent's latest reported state (`state.json`), if any.
pub fn state(game_dir: &Path) -> Result<Value, String> {
    let text = fs::read_to_string(ipc_dir(game_dir).join("state.json"))
        .map_err(|e| format!("no state yet ({e}) — is the game running with the agent?"))?;
    serde_json::from_str(&text).map_err(|e| format!("parse state.json: {e}"))
}

/// One party member from the agent's snapshot.
#[derive(Clone, Debug)]
pub struct Member {
    pub id: i64,
    pub name: String,
    pub level: i64,
    pub hp: i64,
    pub mhp: i64,
    pub mp: i64,
    pub mmp: i64,
    pub tp: i64,
}

/// A parsed, typed view of the agent's reported state.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub ts: u64,
    pub rpgmaker: bool,
    pub engine: String,
    pub title: String,
    pub gold: Option<i64>,
    pub party: Vec<Member>,
    pub freezes: Vec<String>,
    /// Whether a battle is currently active (win/lose apply only then).
    pub in_battle: bool,
    /// Whether the game's update loop is stopped — usually an unhandled
    /// exception, i.e. a candidate for "recover".
    pub stopped: bool,
    /// The last exception message the agent captured, if any.
    pub error: Option<String>,
    /// Checksum of the agent actually running in the game (may differ from the
    /// on-disk plugin until the game is restarted).
    pub agent_hash: Option<String>,
}

/// Read and parse the agent's state into typed fields.
pub fn snapshot(game_dir: &Path) -> Result<Snapshot, String> {
    let v = state(game_dir)?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let party = v
        .get("party")
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter()
                .map(|m| {
                    let n = |k: &str| m.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
                    Member {
                        id: n("id"),
                        name: m.get("name").and_then(|x| x.as_str()).unwrap_or("?").to_string(),
                        level: n("level"),
                        hp: n("hp"),
                        mhp: n("mhp"),
                        mp: n("mp"),
                        mmp: n("mmp"),
                        tp: n("tp"),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Snapshot {
        ts: v.get("ts").and_then(|x| x.as_u64()).unwrap_or(0),
        rpgmaker: v.get("rpgmaker").and_then(|x| x.as_bool()).unwrap_or(false),
        engine: s("engine"),
        title: s("title"),
        gold: v.get("gold").and_then(|x| x.as_i64()),
        party,
        freezes: v
            .get("freezes")
            .and_then(|f| f.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        in_battle: v.get("inBattle").and_then(|x| x.as_bool()).unwrap_or(false),
        stopped: v.get("stopped").and_then(|x| x.as_bool()).unwrap_or(false),
        error: v.get("error").and_then(|x| x.as_str()).map(String::from),
        agent_hash: v.get("agentHash").and_then(|x| x.as_str()).map(String::from),
    })
}

/// A database entry (item / weapon / armor) with the amount currently owned.
#[derive(Clone, Debug)]
pub struct DbEntry {
    pub id: i64,
    pub name: String,
    pub count: i64,
}

/// A named variable or switch with its current value.
#[derive(Clone, Debug)]
pub struct NamedEntry {
    pub id: i64,
    pub name: String,
    pub value: Value,
}

/// The discoverable, modifiable database of an RPG Maker game.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub items: Vec<DbEntry>,
    pub weapons: Vec<DbEntry>,
    pub armors: Vec<DbEntry>,
    pub variables: Vec<NamedEntry>,
    pub switches: Vec<NamedEntry>,
}

/// Which database an item belongs to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Item,
    Weapon,
    Armor,
}

impl ItemKind {
    fn data_array(self) -> &'static str {
        match self {
            ItemKind::Item => "$dataItems",
            ItemKind::Weapon => "$dataWeapons",
            ItemKind::Armor => "$dataArmors",
        }
    }
}

/// Ask the agent to (re)build the catalog into `query.json`.
pub fn request_catalog(game_dir: &Path) -> Result<(), String> {
    send(game_dir, json!([{ "type": "catalog" }]))
}

/// Timestamp (agent `Date.now()` ms) of the catalog the agent last wrote.
/// Lets a caller tell a freshly rebuilt catalog from a stale leftover.
pub fn catalog_ts(game_dir: &Path) -> Option<u64> {
    let text = fs::read_to_string(ipc_dir(game_dir).join("query.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    if v.get("key").and_then(|k| k.as_str()) != Some("catalog") {
        return None;
    }
    v.get("ts").and_then(|x| x.as_u64())
}

/// Read the catalog the agent last wrote, if present and current.
pub fn read_catalog(game_dir: &Path) -> Option<Catalog> {
    let text = fs::read_to_string(ipc_dir(game_dir).join("query.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    if v.get("key").and_then(|k| k.as_str()) != Some("catalog") {
        return None;
    }
    let val = v.get("value")?;
    let db = |key: &str| -> Vec<DbEntry> {
        val.get(key)
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|e| DbEntry {
                        id: e.get("id").and_then(|x| x.as_i64()).unwrap_or(0),
                        name: e.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        count: e.get("count").and_then(|x| x.as_i64()).unwrap_or(0),
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let named = |key: &str| -> Vec<NamedEntry> {
        val.get(key)
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|e| NamedEntry {
                        id: e.get("id").and_then(|x| x.as_i64()).unwrap_or(0),
                        name: e.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        value: e.get("value").cloned().unwrap_or(Value::Null),
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    Some(Catalog {
        items: db("items"),
        weapons: db("weapons"),
        armors: db("armors"),
        variables: named("variables"),
        switches: named("switches"),
    })
}

/// Give (or take, with a negative count) an item/weapon/armor by database id.
pub fn gain_item(game_dir: &Path, kind: ItemKind, id: i64, count: i64) -> Result<(), String> {
    eval_js(
        game_dir,
        &format!("$gameParty.gainItem({}[{id}], {count})", kind.data_array()),
    )
}

/// Set a game variable.
pub fn set_variable(game_dir: &Path, id: i64, value: Value) -> Result<(), String> {
    eval_js(game_dir, &format!("$gameVariables.setValue({id}, {value})"))
}

/// Set a game switch on/off.
pub fn set_switch(game_dir: &Path, id: i64, on: bool) -> Result<(), String> {
    eval_js(game_dir, &format!("$gameSwitches.setValue({id}, {on})"))
}

/// Force the current battle to a victory (only takes effect while in battle).
pub fn battle_win(game_dir: &Path) -> Result<(), String> {
    send(game_dir, json!([{ "type": "battle_win" }]))
}

/// Force the current battle to a defeat.
pub fn battle_lose(game_dir: &Path) -> Result<(), String> {
    send(game_dir, json!([{ "type": "battle_lose" }]))
}

/// Attempt to resume the game after a crash/frozen update loop.
pub fn recover(game_dir: &Path) -> Result<(), String> {
    send(game_dir, json!([{ "type": "recover" }]))
}

// --- Typed command helpers (so callers never build CDP/agent JSON) ----------

/// Set a JS lvalue to a JSON value once.
pub fn set_expr(game_dir: &Path, expr: &str, value: Value) -> Result<(), String> {
    send(game_dir, json!([{ "type": "set", "expr": expr, "value": value }]))
}

/// Run arbitrary JavaScript once.
pub fn eval_js(game_dir: &Path, js: &str) -> Result<(), String> {
    send(game_dir, json!([{ "type": "eval", "expr": js }]))
}

/// Freeze a JS lvalue at a fixed JSON value (re-applied every tick).
pub fn freeze_value(game_dir: &Path, key: &str, expr: &str, value: Value) -> Result<(), String> {
    send(
        game_dir,
        json!([{ "type": "freeze", "key": key, "expr": expr, "value": value }]),
    )
}

/// Freeze by running a JS statement every tick (e.g. keep a stat at its max).
pub fn freeze_run(game_dir: &Path, key: &str, run: &str) -> Result<(), String> {
    send(game_dir, json!([{ "type": "freeze", "key": key, "run": run }]))
}

pub fn unfreeze(game_dir: &Path, key: &str) -> Result<(), String> {
    send(game_dir, json!([{ "type": "unfreeze", "key": key }]))
}

/// Stable key used for a "keep stat at max" freeze.
pub fn max_stat_key(stat: &str, all: bool) -> String {
    format!("max_{stat}{}", if all { "_all" } else { "" })
}

/// Freeze an actor stat (`hp`/`mp`/`tp`) at its maximum, re-derived each tick so
/// it survives level-ups. Standard MV/MZ fields only. Returns the freeze key.
pub fn freeze_stat_max(game_dir: &Path, stat: &str, all: bool) -> Result<String, String> {
    let assign = match stat {
        "hp" => "a._hp=a.mhp;",
        "mp" => "a._mp=a.mmp;",
        "tp" => "a._tp=a.maxTp();",
        other => return Err(format!("unknown stat {other:?}")),
    };
    let run = if all {
        format!("$gameParty.members().forEach(function(a){{{assign}}});")
    } else {
        format!("(function(){{var a=$gameParty.leader();{assign}}})();")
    };
    let key = max_stat_key(stat, all);
    freeze_run(game_dir, &key, &run)?;
    Ok(key)
}
