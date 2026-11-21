//! Dev tool: feed one synthetic situation to a named policy and print the action.
//!   poltest <map_dir> <policy> <scenario>
use mechbot::brain::{Brain, Policy};
use serde_json::json;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let hello = json!({"type": "hello", "slot": 0, "seed": 5, "map": {"dir": args[1]}, "config": {"loot": {"pickup_radius": 4}}});
    let mut b = Brain::new(&hello);
    b.pol = Policy::named(&args[2]);
    let gun = json!({"kind": "gun", "cooldown": 0, "ammo": 12, "reload": 0, "stats": {"damage": 45, "cooldown": 0.24, "speed": 252, "range": 230, "mag": 12, "reload": 2.4}});
    let laser = json!({"kind": "laser", "cooldown": 0, "charge": 0, "stats": {"damage": 315, "charge": 0.75, "cooldown": 1.65, "range": 280}});
    let light = json!({"kind": "lightning", "cooldown": 0, "stats": {"damage": 60, "cooldown": 0.25, "range": 45, "cone_deg": 30, "energy": 3}});
    let (w0, w1, pick_kind) = match args[3].as_str() {
        "gun+laser,lightning-near" => (gun.clone(), laser.clone(), "lightning"),
        "gun+lightning,laser-near" => (gun.clone(), light.clone(), "laser"),
        "laser+lightning,laser-near" => (laser.clone(), light.clone(), "laser"),
        "gun+none,lightning-near" => (gun.clone(), serde_json::Value::Null, "lightning"),
        _ => (gun.clone(), serde_json::Value::Null, "mortar"),
    };
    for k in 0..3 {
        let obs = json!({"type": "obs", "tick": 100 + 2 * k, "t": 5.0 + 0.1 * k as f64, "alive": 16,
            "self": {"id": 0, "pos": [0.0, -60.0, 0.0], "vel": [0, 0, 0], "aim": [0, 0], "hp": 1400, "max_hp": 1400, "energy": 100, "max_energy": 100, "grounded": true, "cubes": 0, "upgrades": [], "in_zone": true,
                "weapons": [w0, w1], "stats": {"walk_speed": 11.7, "height": 9, "muzzle_height": 7, "eye_height": 8}},
            "mechs": [], "projectiles": [], "crates": [], "cubes": [], "pickups": [{"id": 8001, "kind": pick_kind, "pos": [2.0, -60.0, 0.0]}],
            "sounds": [], "beams": [], "events": [], "drops": [],
            "zone": {"center": [41.3, -72.8], "radius": 560, "next_center": [0, -60], "next_radius": 380, "shrink_start": 60, "shrink_end": 110, "dps": 20}, "offer": null});
        let a = b.decide(&obs);
        println!("{} / {}: take={} drop={} label={}", args[2], args[3], a["take"], a["drop"], b.label);
    }
}
