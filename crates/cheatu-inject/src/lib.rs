//! JavaScript-injection backend for NW.js games (RPG Maker MV/MZ, Electron).
//!
//! Instead of scanning and freezing raw memory — which fights V8's garbage
//! collector and destabilizes the game — this talks to the game's own
//! JavaScript runtime over the **Chrome DevTools Protocol** (CDP) and reads or
//! sets values through the game's data model (`$gameParty`, `$gameActors`,
//! `$gameVariables`, …). That's how tools like MTool stay stable.
//!
//! The target must expose a DevTools endpoint, i.e. be launched with
//! `--remote-debugging-port=<port>`. [`config`] can enable that in an NW.js
//! game's `package.json`.

pub mod agent;
pub mod cdp;
pub mod config;
pub mod detect;
pub mod rpgmaker;

pub use cdp::Cdp;
pub use detect::{detect_dir, find_rpgmaker_games, scan_dir, Engine, FoundGame};
pub use rpgmaker::RpgMaker;
