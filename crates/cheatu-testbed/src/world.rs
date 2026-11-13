//! Game state, laid out in memory on purpose.
//!
//! - `GOLD`, `GOLD_SHOWN` (non-zero, `.data`) and `KILLS` (zero, `.bss`) are
//!   statics, so `"cheatu-testbed"+0x…` finds them after a restart.
//! - Everything else hangs off `WORLD`: `[WORLD] → World`, `[World+0] → Level`,
//!   `[Level+8] → Player`, then fields. `next` and death reallocate `Level`,
//!   `Player` and `Inventory`, so their addresses move but the chain holds.
//! - `Player` starts with a 16-byte marker for `aob(…)`. It is stored XOR'd in
//!   the binary and decoded byte by byte into the heap, and freed copies are
//!   zeroed, so a live process holds exactly one copy.
//!
//! The game is single-threaded; `WORLD` is set once by [`init`].

use std::hint::black_box;
use std::ptr::{read_volatile, write_volatile};
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering::Relaxed};

const START_GOLD: u32 = 100;
const START_HP: i32 = 100;
const START_MANA: f32 = 50.0;
const START_AMMO: i16 = 10;

/// `b"CHEATU-TESTBED!!"`, each byte XOR'd with [`MARKER_KEY`].
const MARKER_XORED: [u8; 16] = [
    0xe6, 0xed, 0xe0, 0xe4, 0xf1, 0xf0, 0x88, 0xf1, 0xe0, 0xf6, 0xf1, 0xe7, 0xe0, 0xe1, 0x84, 0x84,
];
const MARKER_KEY: u8 = 0xa5;

const INV_HEAD: [u8; 8] = *b"\xC4\xE7INV\x1D\x9A\x3B";
const INV_TAIL: [u8; 6] = *b"\x5E\xA1\x07\x77\xD2\x0C";

static GOLD: AtomicU32 = AtomicU32::new(START_GOLD);
static KILLS: AtomicU64 = AtomicU64::new(0);
static GOLD_SHOWN: AtomicU32 = AtomicU32::new(START_GOLD);
static WORLD: AtomicPtr<World> = AtomicPtr::new(std::ptr::null_mut());

/// Names reported by [`addrs`], in order.
pub const NAMES: [&str; 9] = [
    "gold",
    "kills",
    "gold_shown",
    "hp",
    "mana",
    "xp",
    "ammo",
    "marker",
    "world",
];

#[repr(C)]
struct World {
    level: Box<Level>,
    /// Allocations kept alive so a new level never reuses the old addresses.
    // Separate allocations on purpose, hence the boxes.
    #[allow(clippy::vec_box)]
    padding: Vec<Box<[u8; 256]>>,
}

#[repr(C)]
struct Level {
    number: u32,
    player: Box<Player>,
}

#[repr(C)]
struct Player {
    marker: [u8; 16],
    hp: i32,
    max_hp: i32,
    mana: f32,
    xp: f64,
    inventory: Box<Inventory>,
}

#[repr(C)]
struct Inventory {
    head: [u8; 8],
    ammo: i16,
    tail: [u8; 6],
}

impl Drop for Player {
    fn drop(&mut self) {
        for b in &mut self.marker {
            // SAFETY: `b` is a valid, exclusive reference.
            unsafe { write_volatile(b, 0) };
        }
    }
}

impl Drop for Inventory {
    fn drop(&mut self) {
        for b in self.head.iter_mut().chain(self.tail.iter_mut()) {
            // SAFETY: `b` is a valid, exclusive reference.
            unsafe { write_volatile(b, 0) };
        }
    }
}

pub enum Outcome {
    Ok,
    Died,
    Refused(&'static str),
}

pub enum Action {
    Fight,
    Heal,
    Shoot,
    Buy,
    Next,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Values {
    pub gold: u32,
    pub kills: u64,
    pub gold_shown: u32,
    pub hp: i32,
    pub mana: f32,
    pub xp: f64,
    pub ammo: i16,
    pub level: u32,
}

/// Allocate a level, with padding allocations in between so its addresses
/// differ from the level it replaces (which is still alive).
#[allow(clippy::vec_box)]
fn new_level(world_padding: &mut Vec<Box<[u8; 256]>>, number: u32, carry: &Player) -> Box<Level> {
    world_padding.push(Box::new([0; 256]));
    world_padding.push(Box::new([0; 256]));
    let inventory = Box::new(Inventory {
        head: INV_HEAD,
        ammo: carry.inventory.ammo,
        tail: INV_TAIL,
    });
    world_padding.push(Box::new([0; 256]));
    world_padding.push(Box::new([0; 256]));
    let mut player = Box::new(Player {
        marker: [0; 16],
        hp: carry.hp,
        max_hp: carry.max_hp,
        mana: carry.mana,
        xp: carry.xp,
        inventory,
    });
    // Decode in place: the plain marker never exists outside the heap. The
    // key goes through `black_box` so the decode is not folded into a plain
    // constant in the binary.
    let key = black_box(MARKER_KEY);
    for (dst, src) in player.marker.iter_mut().zip(MARKER_XORED) {
        // SAFETY: `dst` is a valid, exclusive reference.
        unsafe { write_volatile(dst, src ^ key) };
    }
    Box::new(Level { number, player })
}

fn start_player() -> Player {
    Player {
        marker: [0; 16],
        hp: START_HP,
        max_hp: START_HP,
        mana: START_MANA,
        xp: 0.0,
        inventory: Box::new(Inventory {
            head: [0; 8],
            ammo: START_AMMO,
            tail: [0; 6],
        }),
    }
}

fn world() -> &'static mut World {
    let p = WORLD.load(Relaxed);
    assert!(!p.is_null(), "world::init not called");
    // SAFETY: `p` comes from a leaked `Box` set once in `init`, and the game
    // is single-threaded, so no other reference is alive.
    unsafe { &mut *p }
}

/// Allocate the starting world. Calling it again does nothing.
pub fn init() {
    if !WORLD.load(Relaxed).is_null() {
        return;
    }
    let mut padding = Vec::with_capacity(64);
    let level = new_level(&mut padding, 1, &start_player());
    WORLD.store(Box::into_raw(Box::new(World { level, padding })), Relaxed);
}

fn reset() {
    GOLD.store(START_GOLD, Relaxed);
    KILLS.store(0, Relaxed);
    let w = world();
    w.padding.clear();
    w.level = new_level(&mut w.padding, 1, &start_player());
}

/// Perform `a`. Refused actions change nothing. hp at or below zero resets
/// the game. Afterwards the decoy `gold_shown` is copied from `gold`.
pub fn apply(a: Action) -> Outcome {
    let w = world();
    let p = &mut w.level.player;
    let gold = GOLD.load(Relaxed);
    let mut outcome = Outcome::Ok;
    match a {
        Action::Fight => {
            p.hp -= 7;
            p.xp += 12.5;
            KILLS.fetch_add(1, Relaxed);
            GOLD.store(gold.wrapping_add(15), Relaxed);
        }
        Action::Heal if p.mana < 10.0 => outcome = Outcome::Refused("not enough mana"),
        Action::Heal => {
            p.mana -= 10.0;
            p.hp = (p.hp + 20).min(p.max_hp);
        }
        Action::Shoot if p.inventory.ammo <= 0 => outcome = Outcome::Refused("no ammo"),
        Action::Shoot => p.inventory.ammo -= 1,
        Action::Buy if gold < 20 => outcome = Outcome::Refused("not enough gold"),
        Action::Buy => {
            GOLD.store(gold - 20, Relaxed);
            p.inventory.ammo = p.inventory.ammo.saturating_add(5);
        }
        Action::Next => {
            let number = w.level.number + 1;
            let mut level = new_level(&mut w.padding, number, &w.level.player);
            level.player.mana += 25.0;
            w.level = level;
        }
    }
    if w.level.player.hp <= 0 {
        reset();
        outcome = Outcome::Died;
    }
    refresh_shown();
    outcome
}

/// Copy `gold` into the decoy `gold_shown`, like a game redrawing its HUD.
/// Called before every command and after every action.
pub fn refresh_shown() {
    GOLD_SHOWN.store(GOLD.load(Relaxed), Relaxed);
}

/// The current values, read from memory.
pub fn values() -> Values {
    let level = &world().level;
    let p = &level.player;
    Values {
        gold: GOLD.load(Relaxed),
        kills: KILLS.load(Relaxed),
        gold_shown: GOLD_SHOWN.load(Relaxed),
        hp: p.hp,
        mana: p.mana,
        xp: p.xp,
        ammo: p.inventory.ammo,
        level: level.number,
    }
}

/// The true current address of each of [`NAMES`].
pub fn addrs() -> Vec<(&'static str, u64)> {
    let p = &world().level.player;
    let a = [
        GOLD.as_ptr() as u64,
        KILLS.as_ptr() as u64,
        GOLD_SHOWN.as_ptr() as u64,
        &p.hp as *const i32 as u64,
        &p.mana as *const f32 as u64,
        &p.xp as *const f64 as u64,
        &p.inventory.ammo as *const i16 as u64,
        p.marker.as_ptr() as u64,
        &WORLD as *const AtomicPtr<World> as u64,
    ];
    NAMES.into_iter().zip(a).collect()
}

/// The marker as lowercase hex. Read byte by byte so no plain copy of the
/// marker lands on the stack.
pub fn marker_hex() -> String {
    let p = &world().level.player;
    let mut s = String::with_capacity(32);
    for b in &p.marker {
        // SAFETY: `b` is a valid reference.
        let b = unsafe { read_volatile(b) };
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: Values = Values {
        gold: 100,
        kills: 0,
        gold_shown: 100,
        hp: 100,
        mana: 50.0,
        xp: 0.0,
        ammo: 10,
        level: 1,
    };

    fn addr(name: &str) -> u64 {
        addrs().into_iter().find(|(n, _)| *n == name).unwrap().1
    }

    fn die() {
        while !matches!(apply(Action::Fight), Outcome::Died) {}
    }

    // The game state is global, so the scenarios run in one test, in order.
    #[test]
    fn world_scenarios() {
        init();
        starting_values();
        fight_effects();
        die();
        refused_actions_change_nothing();
        die();
        heal_caps_at_max();
        next_moves_heap_keeps_values();
        death_resets();
        marker_survives_next_and_death();
    }

    fn starting_values() {
        assert_eq!(values(), START);
    }

    fn fight_effects() {
        assert!(matches!(apply(Action::Fight), Outcome::Ok));
        let v = values();
        assert_eq!(
            (v.hp, v.xp, v.kills, v.gold, v.gold_shown),
            (93, 12.5, 1, 115, 115)
        );
    }

    fn refused_actions_change_nothing() {
        for _ in 0..5 {
            assert!(matches!(apply(Action::Heal), Outcome::Ok));
        }
        let before = values();
        assert!(before.mana < 10.0);
        assert!(matches!(apply(Action::Heal), Outcome::Refused(_)));
        assert_eq!(values(), before);

        for _ in 0..10 {
            assert!(matches!(apply(Action::Shoot), Outcome::Ok));
        }
        let before = values();
        assert_eq!(before.ammo, 0);
        assert!(matches!(apply(Action::Shoot), Outcome::Refused(_)));
        assert_eq!(values(), before);

        while values().gold >= 20 {
            assert!(matches!(apply(Action::Buy), Outcome::Ok));
        }
        let before = values();
        assert!(matches!(apply(Action::Buy), Outcome::Refused(_)));
        assert_eq!(values(), before);
    }

    fn heal_caps_at_max() {
        assert_eq!(values().hp, 100);
        assert!(matches!(apply(Action::Heal), Outcome::Ok));
        assert_eq!(values().hp, 100);
        assert_eq!(values().mana, 40.0);
    }

    fn next_moves_heap_keeps_values() {
        let before_v = values();
        let before: Vec<_> = addrs();
        assert!(matches!(apply(Action::Next), Outcome::Ok));
        let after = values();
        for name in ["hp", "mana", "xp", "ammo", "marker"] {
            let old = before.iter().find(|(n, _)| *n == name).unwrap().1;
            assert_ne!(old, addr(name), "{name} should move");
        }
        for name in ["gold", "kills", "gold_shown", "world"] {
            let old = before.iter().find(|(n, _)| *n == name).unwrap().1;
            assert_eq!(old, addr(name), "{name} should stay");
        }
        assert_eq!(after.mana, before_v.mana + 25.0);
        assert_eq!(after.level, before_v.level + 1);
        assert_eq!(
            Values {
                mana: before_v.mana,
                level: before_v.level,
                ..after
            },
            before_v
        );
    }

    fn death_resets() {
        die();
        assert_eq!(values(), START);
    }

    fn marker_survives_next_and_death() {
        assert_eq!(marker_hex(), "4348454154552d544553544245442121");
        apply(Action::Next);
        assert_eq!(marker_hex(), "4348454154552d544553544245442121");
        die();
        assert_eq!(marker_hex(), "4348454154552d544553544245442121");
    }

    #[test]
    fn addrs_order() {
        let names: Vec<_> = super::NAMES.to_vec();
        assert_eq!(
            names,
            [
                "gold",
                "kills",
                "gold_shown",
                "hp",
                "mana",
                "xp",
                "ammo",
                "marker",
                "world"
            ]
        );
    }
}
