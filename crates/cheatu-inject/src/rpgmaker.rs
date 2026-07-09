//! High-level RPG Maker MV/MZ operations, expressed as JavaScript run in-game.
//!
//! These go through the engine's own data model, so the game stays consistent —
//! no GC fighting, no heap corruption, unlike freezing a raw address.

use crate::cdp::Cdp;

/// Convenience wrapper around a [`Cdp`] session for RPG Maker games.
pub struct RpgMaker<'a> {
    cdp: &'a mut Cdp,
}

impl<'a> RpgMaker<'a> {
    pub fn new(cdp: &'a mut Cdp) -> Self {
        RpgMaker { cdp }
    }

    /// Whether the RPG Maker runtime is loaded (a game is in progress).
    pub fn is_present(&mut self) -> bool {
        self.cdp
            .eval_bool("typeof $gameParty !== 'undefined' && $gameParty !== null")
            .unwrap_or(false)
    }

    /// `"MV"`, `"MZ"`, or empty if unknown.
    pub fn engine_name(&mut self) -> String {
        self.cdp
            .eval_string("(typeof Utils!=='undefined' && Utils.RPGMAKER_NAME) || ''")
            .unwrap_or_default()
    }

    /// The game's title from its system data.
    pub fn game_title(&mut self) -> String {
        self.cdp
            .eval_string(
                "(typeof $dataSystem!=='undefined' && $dataSystem && $dataSystem.gameTitle) || ''",
            )
            .unwrap_or_default()
    }

    pub fn gold(&mut self) -> Result<f64, String> {
        self.cdp.eval_f64("$gameParty.gold()")
    }

    /// Set party gold (clamped to the engine's max, like the game would).
    pub fn set_gold(&mut self, amount: i64) -> Result<(), String> {
        self.cdp
            .eval(&format!(
                "$gameParty._gold = Math.min({amount}, $gameParty.maxGold())"
            ))
            .map(|_| ())
    }

    pub fn variable(&mut self, id: u32) -> Result<f64, String> {
        self.cdp.eval_f64(&format!("$gameVariables.value({id})"))
    }

    pub fn set_variable(&mut self, id: u32, value: f64) -> Result<(), String> {
        self.cdp
            .eval(&format!("$gameVariables.setValue({id}, {value})"))
            .map(|_| ())
    }

    pub fn set_switch(&mut self, id: u32, on: bool) -> Result<(), String> {
        self.cdp
            .eval(&format!("$gameSwitches.setValue({id}, {on})"))
            .map(|_| ())
    }

    /// Names of the current party members (index-aligned with `set_actor_hp`).
    pub fn party_members(&mut self) -> Result<Vec<String>, String> {
        let v = self
            .cdp
            .eval("$gameParty.members().map(function(a){return a.name();})")?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .map(|s| s.as_str().unwrap_or("?").to_string())
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Restore HP and MP of every party member to full.
    pub fn heal_party(&mut self) -> Result<(), String> {
        self.cdp
            .eval("$gameParty.members().forEach(function(a){a.recoverAll();})")
            .map(|_| ())
    }

    /// Run an arbitrary JavaScript expression and return it as a string.
    /// Escape hatch for game-specific values (e.g. a custom stamina field).
    pub fn eval(&mut self, expr: &str) -> Result<String, String> {
        Ok(self.cdp.eval(expr)?.to_string())
    }
}
