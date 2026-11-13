//! A saved table finds and writes all three kinds of entry after a restart.

mod common;

use cheatu_core::pointer_scan::{find_chains, PointerMap, PtrScanSettings};
use cheatu_core::table::{self, CheatTable, TableEntry};
use cheatu_core::{make_signature, module_base, AddrExpr, MemSource, ScanProgress, ScanType};
use common::{attach, write, Testbed, MODULE};

fn entry(description: &str, address: AddrExpr, ty: ScanType, value: &str) -> TableEntry {
    TableEntry {
        description: description.into(),
        address,
        ty,
        value: value.into(),
        frozen: false,
    }
}

#[test]
fn table_round_trip_writes_through_each_entry() {
    let mut tb = Testbed::spawn();
    let Some(src) = attach(&tb) else { return };

    let base = module_base(src.regions(), MODULE).unwrap();
    let gold = AddrExpr::parse(&format!("\"{MODULE}\"+0x{:x}", tb.addr("gold") - base)).unwrap();

    let settings = PtrScanSettings {
        max_results: 10_000,
        ..PtrScanSettings::default()
    };
    let progress = ScanProgress::default();
    let map = PointerMap::build(&src, &settings, &progress).unwrap();
    let chains = find_chains(&map, &src, tb.addr("hp"), &settings, &progress).unwrap();
    // Shortest first; take the shortest chain rooted in the testbed itself.
    let hp = chains
        .chains
        .iter()
        .find(|c| c.module == MODULE)
        .expect("a chain rooted in the testbed")
        .to_expr();
    drop(map);

    let ammo = make_signature(&src, tb.addr("ammo"), 64).unwrap();

    let t = CheatTable {
        process: Some(MODULE.into()),
        entries: vec![
            entry("Gold", gold, ScanType::U32, "100"),
            entry("HP", hp, ScanType::I32, "100"),
            entry("Ammo", ammo, ScanType::I16, "10"),
        ],
    };
    let path = std::env::temp_dir().join(format!("cheatu-testbed-{}.toml", std::process::id()));
    table::save(&t, &path).unwrap();
    drop((src, tb));

    let mut tb = Testbed::spawn();
    let src = attach(&tb).unwrap();
    let loaded = table::load(&path);
    let _ = std::fs::remove_file(&path);
    let loaded = loaded.unwrap();
    assert_eq!(loaded, t);

    let writes: [(&str, Vec<u8>, &str); 3] = [
        ("gold", 4242u32.to_ne_bytes().to_vec(), "4242"),
        ("hp", 77i32.to_ne_bytes().to_vec(), "77"),
        ("ammo", 99i16.to_ne_bytes().to_vec(), "99"),
    ];
    for (e, (name, bytes, shown)) in loaded.entries.iter().zip(writes) {
        let addr = e.address.resolve(&src).unwrap();
        assert_eq!(addr, tb.addr(name), "{}: {}", e.description, e.address);
        write(&tb, addr, &bytes);
        assert_eq!(tb.get(name), shown, "{}", e.description);
    }
}
