//! What a player standing at a spot in the Rogue Encampment is sent, room by room:
//! `town_units <data dir> <Game.exe> <map seed> <x> <y>`.

use d2_data::engine::EngineData;
use d2_data::GameData;
use d2_drlg::act::Act;
use d2_drlg::preset::PresetLevel;
use d2_drlg::world::RoomId;
use d2_game::population::{Population, Spawned};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, dir, exe, seed, x, y] = args.as_slice() else {
        eprintln!("usage: town_units <data dir> <Game.exe> <map seed> <x> <y>");
        std::process::exit(2);
    };
    let number = |s: &str| s.strip_prefix("0x").map_or_else(|| s.parse(), |h| u32::from_str_radix(h, 16)).expect("a number");
    let data = GameData::load(dir).expect("install");
    let engine = EngineData::from_game_exe(&std::fs::read(exe).expect("Game.exe")).expect("1.14d");
    let act = Act::build(data.levels(), 0, 0, number(seed));
    let level = PresetLevel::build(&data, &engine, &act, 1).expect("town");
    let (x, y) = (number(x) as i32, number(y) as i32);
    let Some(room) = level.room_index_at(x, y) else {
        eprintln!("({x}, {y}) is not in the town {:?}", level.area);
        std::process::exit(1);
    };
    println!("{} area {:?}; ({x}, {y}) in room {:?}", level.map, level.area, level.rooms[room]);
    let mut pop = Population::new(1);
    for near in level.rooms_near(room) {
        println!("room {:?}", level.rooms[near]);
        let a = pop.activate(&data, level.level_id, RoomId { level: level.level_id, index: near }, level.units_in(near), None);
        for u in a.units {
            match *u {
                Spawned::Object { guid, class, x, y, mode, .. } => {
                    println!("  object  guid {guid:3} class {class:3} {:24} ({x}, {y}) mode {mode}", data.objects().name(class.into()).unwrap_or("?"));
                }
                Spawned::Monster { guid, class, x, y, components, .. } => {
                    let name = data.monsters().get(class.into()).map_or("?", |m| m.id.as_str());
                    println!("  monster guid {guid:3} class {class:3} {name:24} ({x}, {y}) components {components:?}");
                }
            }
        }
        for n in &a.not_ported {
            println!("  not ported: {n}");
        }
    }
}
