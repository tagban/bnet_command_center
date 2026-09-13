//! List a DS1's preset units from an install: `ds1_units <data dir> <member>`.

use d2_formats::ds1::Ds1;
use d2_formats::mpq::{ArchiveSet, DATA_ARCHIVES};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, dir, member] = args.as_slice() else {
        eprintln!("usage: ds1_units <data dir> <member>");
        std::process::exit(2);
    };
    let set = ArchiveSet::open(dir, &DATA_ARCHIVES).expect("install");
    let bytes = set.read(member).expect("read").expect("no such member");
    let ds1 = Ds1::parse(&bytes).expect("parse");
    println!("version {} size {}x{} act {} units {}", ds1.version, ds1.width, ds1.height, ds1.act, ds1.units.len());
    for u in &ds1.units {
        println!("{:?} id {} at ({}, {}) flags {} path {}", u.kind, u.id, u.x, u.y, u.flags, u.path.len());
    }
}
