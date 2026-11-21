//! Decision making: world memory, zone timing, threat handling, looting, movement and aim.
//!
//! Priorities, highest first:
//!   1. stay inside the zone (rotate early, boost when late)
//!   2. survive contact: fight only with a clear advantage, otherwise break line of sight
//!   3. loot when nobody is near (second weapon, cubes, crates, drones, supply drops)
//!   4. hold a covered spot near the edge of the next circle and keep scanning

use crate::map::{Map, Planner};
use serde_json::{json, Value};
// BTreeMap keeps iteration order (and so every tie-break) the same from run to run.
use std::collections::BTreeMap as HashMap;
use std::f32::consts::{PI, TAU};

fn fnum(v: &Value) -> f32 {
    v.as_f64().unwrap_or(0.0) as f32
}
fn fnum_or(v: &Value, d: f32) -> f32 {
    v.as_f64().map(|x| x as f32).unwrap_or(d)
}
fn v2(v: &Value) -> [f32; 2] {
    [fnum(&v[0]), fnum(&v[1])]
}
fn v3(v: &Value) -> [f32; 3] {
    [fnum(&v[0]), fnum(&v[1]), fnum(&v[2])]
}
fn wrap(a: f32) -> f32 {
    if !a.is_finite() {
        return 0.0;
    }
    let mut a = a % TAU;
    if a > PI {
        a -= TAU;
    }
    if a < -PI {
        a += TAU;
    }
    a
}
fn d2(a: &[f32], b: &[f32]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}
fn d3(a: &[f32; 3], b: &[f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}
fn r4(x: f32) -> f64 {
    if x.is_finite() {
        ((x as f64) * 10000.0).round() / 10000.0
    } else {
        0.0
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum WK {
    Gun,
    Mortar,
    Laser,
    Lightning,
}
fn wk(s: &str) -> Option<WK> {
    match s {
        "gun" => Some(WK::Gun),
        "mortar" => Some(WK::Mortar),
        "laser" => Some(WK::Laser),
        "lightning" => Some(WK::Lightning),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct Wpn {
    kind: WK,
    cooldown: f32,
    ammo: i32,
    reload: f32,
    charge: f32,
    speed: f32,
    range: f32,
    mag: i32,
    charge_t: f32,
    cone: f32,
}
fn parse_wpn(w: &Value) -> Option<Wpn> {
    let kind = wk(w["kind"].as_str()?)?;
    let st = &w["stats"];
    Some(Wpn {
        kind,
        cooldown: fnum(&w["cooldown"]),
        ammo: w["ammo"].as_i64().unwrap_or(0) as i32,
        reload: fnum(&w["reload"]),
        charge: fnum(&w["charge"]),
        speed: fnum_or(&st["speed"], 252.0).max(1.0),
        range: fnum_or(&st["range"], 200.0),
        mag: st["mag"].as_i64().unwrap_or(12) as i32,
        charge_t: fnum_or(&st["charge"], 0.75),
        cone: fnum_or(&st["cone_deg"], 30.0),
    })
}

#[derive(Clone)]
struct Me {
    pos: [f32; 3],
    vel: [f32; 3],
    aim: [f32; 2],
    hp: f32,
    max_hp: f32,
    energy: f32,
    grounded: bool,
    cubes: i32,
    in_zone: bool,
    w: [Option<Wpn>; 2],
    walk: f32,
    muzzle_h: f32,
    eye_h: f32,
    height: f32,
    min_pitch: f32,
    max_pitch: f32,
}

#[derive(Clone)]
struct Track {
    pos: [f32; 3],
    vel: [f32; 3],
    aim: [f32; 2],
    hp: f32,
    cubes: i32,
    gun: bool,
    laser: bool,
    lightning: bool,
    mortar: bool,
    charging: bool,
    seen_t: f32,
    hit_me_t: f32,
}
#[derive(Clone)]
struct Npc {
    pos: [f32; 3],
    vel: [f32; 3],
    seen_t: f32,
}
#[derive(Clone)]
struct Item {
    pos: [f32; 3],
    kind: Option<WK>,
    seen_t: f32,
}
#[derive(Clone)]
struct DropObs {
    id: i64,
    pos: [f32; 3],
    lands_in: f32,
    holder: Option<i64>,
}
#[derive(Clone, Default)]
struct Zone {
    c: [f32; 2],
    r: f32,
    nc: [f32; 2],
    nr: f32,
    s: f32,
    e: f32,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum LK {
    Pickup,
    Cube,
    Crate,
    Npc,
    Drop,
}
#[derive(Clone, Copy)]
struct Loot {
    kind: LK,
    id: i64,
    pos: [f32; 3],
}

#[derive(Clone, Copy)]
struct Th {
    id: i64,
    pos: [f32; 3],
    vel: [f32; 3],
    d: f32,
    los: bool,
    facing: bool,
    hostile: bool,
    hp: f32,
    dps: f32,
    cubes: i32,
    lightning: bool,
}

#[derive(Clone, Copy)]
struct Tgt {
    pos: [f32; 3],
    vel: [f32; 3],
    r: f32,
    h: f32,
    mech: bool,
}

struct Act {
    mv: [f32; 2],
    boost: bool,
    jump: bool,
    aim: [f32; 2],
    fire: [bool; 2],
    reload: bool,
    pick: Option<i64>,
    take: Option<i64>,
    drop: Option<i64>,
}

/// Behaviour switches. One code base carries every experiment; the simulator can run different
/// policies against each other in the same match.
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    /// Against the practice squad, fight from a roof: climb the nearest big flat roof, stand where
    /// the ground near the building cannot see us, step to the edge to shoot, step back to reload,
    /// and come down when the zone says so.
    pub roof: bool,
    /// Recognise a free-for-all within seconds (gunfire all around, other mechs losing health to
    /// each other, two different attackers, an early death) instead of after two deaths.
    pub ffa_fast: bool,
    /// In the first seconds of a match do not trade shots at range: break line of sight instead.
    pub opening_caution: bool,
    /// When breaking contact, run to the nearest spot the shooters cannot see (not to the
    /// best-scoring spot, which could be a long run across open ground).
    pub near_cover: bool,
    /// In a free-for-all, get out of sight of any mech that can see us, whether or not it has
    /// started shooting.
    pub unseen: bool,
    /// In a free-for-all, hide in the middle of a nearby roof (no peeking, no patrol; shoot back
    /// only at a mech that is shooting us).
    pub roof_ffa: bool,
    /// Climb over what holds us up: when pressed against something, or when three seconds bring
    /// us no closer to where we are going, jump and jet until the feet clear it (replaces the
    /// one-second hop of version 1). This is what gets the mech out of the stadium bowl.
    pub climb: bool,
    /// Lightning doctrine: the wanted loadout is lightning plus laser (the gun gives way), fights
    /// are taken at 30 m where lightning hits without aiming and chains through a pack, and
    /// upgrades favour lightning and energy.
    pub lightning: bool,
    /// Step out of the path of gun bullets that are already in the air.
    pub dodge: bool,
    /// With an empty weapon slot, fetch a laser or lightning before any other loot.
    pub arm_first: bool,
    /// After a fight, stay hidden until health is back above 85% before looting again.
    pub heal: bool,
    /// With two or more hunters on us, fight from a spot fewer of them can see.
    pub split: bool,
    /// Keep after a wounded practice hunter that breaks contact.
    pub finish: bool,
    /// Break off to regenerate earlier when two or three hunters are on us.
    pub backoff: bool,
    /// Second step on the same dial (0 = off).
    pub backoff2: u8,
    /// When arming, a laser is worth walking up to 120 m further for than a lightning.
    pub laser_pref: bool,
    /// Free-for-all: shoot first at a mech that is busy trading with someone else (losing health
    /// to others, not facing us) while we are healthy.
    pub vulture: bool,
    /// Free-for-all: toughness upgrades (plating, armor, nanites) before damage.
    pub ffa_tough: bool,
    /// Free-for-all: after a fight, stay hidden and regenerate before looting again.
    pub ffa_heal: bool,
    /// Free-for-all: one mech on us and an even trade: shoot back instead of running (running
    /// takes the same damage and deals none).
    pub stand: bool,
    /// Free-for-all: leave crates and drones alone (noise, drone fire, time in the open), and walk
    /// below step-sound speed when the zone allows.
    pub hide: bool,
    /// Free-for-all: cubes are health and damage; take drones, crates and cubes more readily.
    pub farm: bool,
}

impl Policy {
    /// What the submitted binary runs.
    pub fn submitted() -> Policy {
        Policy { roof: false, ffa_fast: true, opening_caution: true, near_cover: true, unseen: true, roof_ffa: false, climb: true, lightning: false, dodge: true, arm_first: true, heal: true, split: true, finish: true, backoff: true, backoff2: 0, laser_pref: true, vulture: false, ffa_tough: false, ffa_heal: false, stand: false, hide: false, farm: false }
    }

    /// Named presets for the simulator.
    pub fn named(name: &str) -> Policy {
        match name {
            "e" => Policy { roof: false, ffa_fast: false, opening_caution: false, near_cover: false, unseen: false, roof_ffa: false, climb: false, lightning: false, dodge: false, arm_first: false, heal: false, split: false, finish: false, backoff: false, backoff2: 0, laser_pref: false, vulture: false, ffa_tough: false, ffa_heal: false, stand: false, hide: false, farm: false },
            "l" => Policy { roof: false, ffa_fast: false, opening_caution: false, near_cover: false, unseen: false, roof_ffa: false, climb: false, lightning: true, dodge: false, arm_first: false, heal: false, split: false, finish: false, backoff: false, backoff2: 0, laser_pref: false, vulture: false, ffa_tough: false, ffa_heal: false, stand: false, hide: false, farm: false },
            "m" => Policy { backoff2: 0, laser_pref: false, ..Policy::submitted() },
            "n1" => Policy { backoff2: 1, ..Policy::submitted() },
            "n2" => Policy { backoff2: 2, ..Policy::submitted() },
            "n3" => Policy { backoff2: 3, ..Policy::submitted() },
            "k" => Policy { roof: false, ffa_fast: true, opening_caution: true, near_cover: true, unseen: true, roof_ffa: false, climb: true, lightning: false, dodge: true, arm_first: true, heal: true, split: true, finish: true, backoff: false, backoff2: 0, laser_pref: false, vulture: false, ffa_tough: false, ffa_heal: false, stand: false, hide: false, farm: false },
            "j" => Policy { roof: false, ffa_fast: true, opening_caution: true, near_cover: true, unseen: true, roof_ffa: false, climb: true, lightning: false, dodge: true, arm_first: true, heal: true, split: true, finish: false, backoff: false, backoff2: 0, laser_pref: false, vulture: false, ffa_tough: false, ffa_heal: false, stand: false, hide: false, farm: false },
            "i" => Policy { roof: false, ffa_fast: true, opening_caution: true, near_cover: true, unseen: true, roof_ffa: false, climb: true, lightning: false, dodge: true, arm_first: true, heal: true, split: false, finish: false, backoff: false, backoff2: 0, laser_pref: false, vulture: false, ffa_tough: false, ffa_heal: false, stand: false, hide: false, farm: false },
            "h" => Policy { roof: false, ffa_fast: true, opening_caution: true, near_cover: true, unseen: true, roof_ffa: false, climb: true, lightning: false, dodge: true, arm_first: true, heal: false, split: false, finish: false, backoff: false, backoff2: 0, laser_pref: false, vulture: false, ffa_tough: false, ffa_heal: false, stand: false, hide: false, farm: false },
            "g" => Policy { roof: false, ffa_fast: true, opening_caution: true, near_cover: true, unseen: true, roof_ffa: false, climb: true, lightning: false, dodge: true, arm_first: false, heal: false, split: false, finish: false, backoff: false, backoff2: 0, laser_pref: false, vulture: false, ffa_tough: false, ffa_heal: false, stand: false, hide: false, farm: false },
            "nd" => Policy { roof: false, ffa_fast: true, opening_caution: true, near_cover: true, unseen: true, roof_ffa: false, climb: true, lightning: false, dodge: false, arm_first: false, heal: false, split: false, finish: false, backoff: false, backoff2: 0, laser_pref: false, vulture: false, ffa_tough: false, ffa_heal: false, stand: false, hide: false, farm: false },
            "ec" => Policy { roof: false, ffa_fast: false, opening_caution: false, near_cover: false, unseen: false, roof_ffa: false, climb: true, lightning: false, dodge: false, arm_first: false, heal: false, split: false, finish: false, backoff: false, backoff2: 0, laser_pref: false, vulture: false, ffa_tough: false, ffa_heal: false, stand: false, hide: false, farm: false },
            "ffa3n" => Policy { roof: false, ffa_fast: true, opening_caution: true, near_cover: true, unseen: true, roof_ffa: false, climb: false, lightning: false, dodge: false, arm_first: false, heal: false, split: false, finish: false, backoff: false, backoff2: 0, laser_pref: false, vulture: false, ffa_tough: false, ffa_heal: false, stand: false, hide: false, farm: false },
            "r" => Policy { roof: true, ffa_fast: false, opening_caution: false, near_cover: false, unseen: false, roof_ffa: false, climb: true, lightning: false, dodge: false, arm_first: false, heal: false, split: false, finish: false, backoff: false, backoff2: 0, laser_pref: false, vulture: false, ffa_tough: false, ffa_heal: false, stand: false, hide: false, farm: false },
            "ffa" => Policy { roof: false, ffa_fast: true, opening_caution: true, near_cover: false, unseen: false, roof_ffa: false, climb: true, lightning: false, dodge: false, arm_first: false, heal: false, split: false, finish: false, backoff: false, backoff2: 0, laser_pref: false, vulture: false, ffa_tough: false, ffa_heal: false, stand: false, hide: false, farm: false },
            "ffa2" => Policy { roof: false, ffa_fast: true, opening_caution: true, near_cover: true, unseen: false, roof_ffa: false, climb: true, lightning: false, dodge: false, arm_first: false, heal: false, split: false, finish: false, backoff: false, backoff2: 0, laser_pref: false, vulture: false, ffa_tough: false, ffa_heal: false, stand: false, hide: false, farm: false },
            "ffa3" => Policy { roof: false, ffa_fast: true, opening_caution: true, near_cover: true, unseen: true, roof_ffa: false, climb: true, lightning: false, dodge: false, arm_first: false, heal: false, split: false, finish: false, backoff: false, backoff2: 0, laser_pref: false, vulture: false, ffa_tough: false, ffa_heal: false, stand: false, hide: false, farm: false },
            "ffa4" => Policy { roof: false, ffa_fast: true, opening_caution: true, near_cover: true, unseen: true, roof_ffa: true, climb: true, lightning: false, dodge: false, arm_first: false, heal: false, split: false, finish: false, backoff: false, backoff2: 0, laser_pref: false, vulture: false, ffa_tough: false, ffa_heal: false, stand: false, hide: false, farm: false },
            "e2" => Policy { roof: false, ffa_fast: false, opening_caution: false, near_cover: true, unseen: false, roof_ffa: false, climb: true, lightning: false, dodge: false, arm_first: false, heal: false, split: false, finish: false, backoff: false, backoff2: 0, laser_pref: false, vulture: false, ffa_tough: false, ffa_heal: false, stand: false, hide: false, farm: false },
            "old" => Policy { vulture: false, ffa_tough: false, ffa_heal: false, stand: false, hide: false, farm: false, ..Policy::submitted() },
            "v" => Policy { vulture: true, ffa_tough: false, ffa_heal: false, stand: false, hide: false, farm: false, ..Policy::submitted() },
            "t" => Policy { vulture: false, ffa_tough: true, ffa_heal: false, stand: false, hide: false, farm: false, ..Policy::submitted() },
            "hl" => Policy { vulture: false, ffa_tough: false, ffa_heal: true, stand: false, hide: false, farm: false, ..Policy::submitted() },
            "new" => Policy { vulture: true, ffa_tough: true, ffa_heal: true, stand: false, hide: false, farm: false, ..Policy::submitted() },
            "st" => Policy { vulture: false, ffa_tough: false, ffa_heal: false, stand: true, hide: false, farm: false, ..Policy::submitted() },
            "hide" => Policy { hide: true, ffa_heal: true, ..Policy::named("old") },
            "farm" => Policy { farm: true, ..Policy::named("old") },
            "vf" => Policy { farm: true, vulture: true, ffa_tough: true, ..Policy::named("old") },
            _ => Policy::submitted(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum RoofSt {
    Off,
    Approach,
    Climb,
    On,
    Leave,
}

// Experiment E: damage first. A bigger magazine is the largest single gain in sustained gun
// damage (12 rounds then a 2.4 s reload is 107 damage a second; 20 rounds and a 1.9 s reload is
// 139), then per-shot damage, fire rate and laser charge; toughness comes after.
// (Versions up to D took armor, plating and nanites first.)
const UPGRADE_ORDER: [&str; 22] = [
    "gun_mag",
    "gun_damage",
    "laser_damage",
    "gun_rate",
    "laser_charge",
    "lightning_efficiency",
    "plating",
    "armor",
    "servos",
    "nanites",
    "gun_velocity",
    "reactor",
    "capacitor",
    "jets",
    "gyros",
    "sensors",
    "lightning_chain",
    "lightning_range",
    "laser_pierce",
    "mortar_reload",
    "mortar_splash",
    "mortar_velocity",
];

// Free-for-all: the teams that place best in the public showdowns take plating, armor and
// nanites first; surviving longer is what scores there.
const FFA_ORDER: [&str; 22] = [
    "plating",
    "armor",
    "nanites",
    "laser_damage",
    "laser_charge",
    "gun_mag",
    "gun_damage",
    "gun_rate",
    "lightning_efficiency",
    "servos",
    "gun_velocity",
    "reactor",
    "capacitor",
    "jets",
    "gyros",
    "sensors",
    "lightning_chain",
    "lightning_range",
    "laser_pierce",
    "mortar_reload",
    "mortar_splash",
    "mortar_velocity",
];

// Upgrade order for the lightning doctrine: lightning itself and the energy it runs on first.
const LIGHTNING_ORDER: [&str; 22] = [
    "lightning_efficiency",
    "lightning_chain",
    "laser_damage",
    "reactor",
    "lightning_range",
    "plating",
    "capacitor",
    "armor",
    "laser_charge",
    "gun_mag",
    "gun_damage",
    "gun_rate",
    "servos",
    "nanites",
    "jets",
    "gyros",
    "gun_velocity",
    "sensors",
    "laser_pierce",
    "mortar_reload",
    "mortar_splash",
    "mortar_velocity",
];

fn dps_of(gun: bool, laser: bool, lightning: bool, mortar: bool, d: f32, cubes: i32) -> f32 {
    let mut s = 0.0;
    if gun && d < 225.0 {
        s += 107.0 * (1.15 - d / 220.0).clamp(0.25, 1.0);
    }
    if laser && d < 275.0 {
        s += 131.0;
    }
    if lightning && d < 43.0 {
        s += 240.0;
    }
    if mortar && d > 12.0 && d < 150.0 {
        s += 50.0;
    }
    s.max(5.0) * (1.0 + 0.04 * cubes.clamp(0, 10) as f32)
}

pub struct Brain {
    pub pol: Policy,
    pub map: Map,
    pl: Planner,
    rng: u64,
    pickup_r: f32,
    t: f32,
    tick: Value,
    alive: i32,
    me: Me,
    zone: Zone,
    mechs: HashMap<i64, Track>,
    npcs: HashMap<i64, Npc>,
    crates: HashMap<i64, Item>,
    cubes: HashMap<i64, Item>,
    pickups: HashMap<i64, Item>,
    drops: Vec<DropObs>,
    /// Enemy bullets in view: position and velocity.
    shots: Vec<([f32; 3], [f32; 3])>,
    pub dodges: u32,
    offer: Option<Vec<String>>,
    last_hit_t: f32,
    last_hit_bearing: f32,
    last_mech_hit_t: f32,
    last_hit_by: i64,
    npc_hit_t: f32,
    npc_hit_by: i64,
    new_contact: bool,
    virt: Option<[f32; 3]>,
    virt_until: f32,
    cmd_aim: [f32; 2],
    path: Vec<u32>,
    path_found: bool,
    path_goal: [f32; 2],
    path_tick: i64,
    spot: Option<[f32; 2]>,
    spot_tick: i64,
    spot_evade: bool,
    spot_nc: [f32; 2],
    last_tgt: i64,
    last_fight_t: f32,
    split_spot: Option<[f32; 2]>,
    split_tick: i64,
    cover: Option<[f32; 2]>,
    cover_tick: i64,
    cover_seen: Vec<u32>,
    cover_epoch: u32,
    loot: Option<Loot>,
    stuck: i32,
    unstick: i32,
    unstick_dir: f32,
    /// Decisions left in the current climb (jump, then jet until the feet clear what is ahead).
    jet: i32,
    jet_from: [f32; 2],
    jet_dir: [f32; 2],
    jet_fails: i32,
    jet_wait: i32,
    /// Progress toward the current walking goal: the goal, the best distance reached and when.
    prog_goal: [f32; 2],
    prog_best: f32,
    prog_since: i64,
    noprog: bool,
    strafe_dir: f32,
    strafe_until: f32,
    fight_until: f32,
    evade_until: f32,
    dcount: i64,
    /// When this decision started (for the time guard).
    tick_start: std::time::Instant,
    roof_st: RoofSt,
    roof: usize,
    roof_foot: [f32; 2],
    roof_land: [f32; 2],
    roof_t0: f32,
    roof_pick_t: f32,
    roof_ban: f32,
    roof_fails: i32,
    /// Roofs we failed to climb: not tried again this match.
    roof_bad: Vec<usize>,
    roof_dir: [f32; 2],
    roof_still: i32,
    patrol_k: usize,
    patrol_t: f32,
    /// Set when the zone forces a move; cleared only once we are properly inside the next circle.
    zone_latch: bool,
    /// False while every death so far was our own kill: the practice format (one squad of house
    /// bots that never fight each other). True once somebody else has killed somebody.
    pub ffa: bool,
    kills: i32,
    start_alive: i32,
    mismatch_t: f32,
    /// Weapon sounds heard that were not a mech shooting at us.
    snd_other: u32,
    /// Mechs that damaged us before our first kill.
    early_attackers: Vec<i64>,
    /// Last health seen per mech, and when we last hit each.
    hp_seen: HashMap<i64, (f32, f32)>,
    my_hit_t: HashMap<i64, f32>,
    /// when each mech last lost health to somebody other than us
    busy_t: HashMap<i64, f32>,
    hp_drops: u32,
    snd_ffa: bool,
    /// A mech that keeps shooting us: its id, since when, and its latest hit.
    pursuer: (i64, f32, f32),
    /// Why we decided this is a free-for-all (for the simulator trace).
    pub ffa_why: &'static str,
    regen_mode: bool,
    pub label: &'static str,
    /// Debug text for the simulator trace.
    pub dbg: String,
    pub gate: [u32; 6],
    pub known_pickups: usize,
    /// Worst time seen per section, in ms: ingest, spot choice, loot choice, path planning, whole decision.
    pub prof: [f32; 5],
}

impl Brain {
    pub fn new(hello: &Value) -> Brain {
        let dir = hello["map"]["dir"].as_str().unwrap_or("/arena");
        let map = match Map::load(dir) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("map load failed ({}); using a flat map", e);
                Map::flat()
            }
        };
        Brain::with_map(map, hello)
    }

    pub fn with_map(map: Map, hello: &Value) -> Brain {
        let cfg = &hello["config"];
        let n = map.gx * map.gy;
        let seed = hello["seed"].as_u64().unwrap_or(1) ^ 0x9E37_79B9_7F4A_7C15;
        Brain {
            pol: Policy::submitted(),
            map,
            pl: Planner::new(n),
            rng: seed | 1,
            pickup_r: fnum_or(&cfg["loot"]["pickup_radius"], 4.0),
            t: 0.0,
            tick: json!(0),
            alive: 16,
            me: Me { pos: [0.0; 3], vel: [0.0; 3], aim: [0.0; 2], hp: 1400.0, max_hp: 1400.0, energy: 100.0, grounded: true, cubes: 0, in_zone: true, w: [None, None], walk: 11.7, muzzle_h: 7.0, eye_h: 8.0, height: 9.0, min_pitch: -0.7, max_pitch: 1.3 },
            zone: Zone::default(),
            mechs: HashMap::new(),
            npcs: HashMap::new(),
            crates: HashMap::new(),
            cubes: HashMap::new(),
            pickups: HashMap::new(),
            drops: Vec::new(),
            shots: Vec::new(),
            dodges: 0,
            offer: None,
            last_hit_t: -100.0,
            last_hit_bearing: 0.0,
            last_mech_hit_t: -100.0,
            last_hit_by: -1,
            npc_hit_t: -100.0,
            npc_hit_by: -1,
            new_contact: false,
            virt: None,
            virt_until: -1.0,
            cmd_aim: [0.0, 0.0],
            path: Vec::new(),
            path_found: false,
            path_goal: [0.0, 0.0],
            path_tick: -100,
            spot: None,
            spot_tick: -100,
            spot_evade: false,
            spot_nc: [f32::NAN, f32::NAN],
            last_tgt: -1,
            last_fight_t: -100.0,
            split_spot: None,
            split_tick: -100,
            cover: None,
            cover_tick: -100,
            cover_seen: vec![0; n],
            cover_epoch: 0,
            loot: None,
            stuck: 0,
            unstick: 0,
            unstick_dir: 0.0,
            jet: 0,
            jet_from: [0.0, 0.0],
            jet_dir: [1.0, 0.0],
            jet_fails: 0,
            jet_wait: 0,
            prog_goal: [0.0, 0.0],
            prog_best: 0.0,
            prog_since: 0,
            noprog: false,
            strafe_dir: 1.0,
            strafe_until: 0.0,
            fight_until: -1.0,
            evade_until: -1.0,
            dcount: 0,
            tick_start: std::time::Instant::now(),
            roof_st: RoofSt::Off,
            roof: 0,
            roof_foot: [0.0, 0.0],
            roof_land: [0.0, 0.0],
            roof_t0: 0.0,
            roof_pick_t: -100.0,
            roof_ban: -1.0,
            roof_fails: 0,
            roof_bad: Vec::new(),
            roof_dir: [1.0, 0.0],
            roof_still: 0,
            patrol_k: 0,
            patrol_t: 0.0,
            zone_latch: false,
            ffa: false,
            kills: 0,
            start_alive: -1,
            mismatch_t: -1.0,
            snd_other: 0,
            early_attackers: Vec::new(),
            hp_seen: HashMap::new(),
            my_hit_t: HashMap::new(),
            busy_t: HashMap::new(),
            hp_drops: 0,
            snd_ffa: false,
            pursuer: (-1, 0.0, -100.0),
            ffa_why: "",
            regen_mode: false,
            label: "",
            prof: [0.0; 5],
            dbg: String::new(),
            gate: [0; 6],
            known_pickups: 0,
        }
    }

    fn rand(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        ((x >> 40) as f32) / ((1u64 << 24) as f32)
    }

    // ------------------------------------------------------------------ observation -> memory
    pub fn ingest(&mut self, o: &Value) {
        let t = fnum(&o["t"]);
        self.t = t;
        self.tick = o["tick"].clone();
        self.alive = o["alive"].as_i64().unwrap_or(16) as i32;
        let s = &o["self"];
        let st = &s["stats"];
        let my_id = s["id"].as_i64().unwrap_or(-1);
        self.me = Me {
            pos: v3(&s["pos"]),
            vel: v3(&s["vel"]),
            aim: v2(&s["aim"]),
            hp: fnum(&s["hp"]),
            max_hp: fnum_or(&s["max_hp"], 1400.0).max(1.0),
            energy: fnum(&s["energy"]),
            grounded: s["grounded"].as_bool().unwrap_or(true),
            cubes: s["cubes"].as_i64().unwrap_or(0) as i32,
            in_zone: s["in_zone"].as_bool().unwrap_or(true),
            w: [parse_wpn(&s["weapons"][0]), parse_wpn(&s["weapons"][1])],
            walk: fnum_or(&st["walk_speed"], 11.7).max(1.0),
            muzzle_h: fnum_or(&st["muzzle_height"], 7.0),
            eye_h: fnum_or(&st["eye_height"], 8.0),
            height: fnum_or(&st["height"], 9.0),
            min_pitch: fnum_or(&st["min_pitch"], -0.7),
            max_pitch: fnum_or(&st["max_pitch"], 1.3),
        };
        let z = &o["zone"];
        if z.is_object() {
            let c = v2(&z["center"]);
            let r = fnum(&z["radius"]);
            let nc = if z["next_center"].is_array() { v2(&z["next_center"]) } else { c };
            let nr = if z["next_radius"].is_number() { fnum(&z["next_radius"]) } else { r };
            self.zone = Zone { c, r, nc, nr, s: fnum_or(&z["shrink_start"], t), e: fnum_or(&z["shrink_end"], t) };
        }
        let eye = [self.me.pos[0], self.me.pos[1], self.me.pos[2] + self.me.eye_h];
        let me_pos = self.me.pos;
        // headroom needed under bridges (the mech grows with cubes)
        self.map.need_h = self.me.height + 0.4;

        // mechs and drones
        let mut seen_npc: Vec<i64> = Vec::new();
        if let Some(arr) = o["mechs"].as_array() {
            for m in arr {
                let id = m["id"].as_i64().unwrap_or(-1);
                if id == my_id {
                    continue;
                }
                let pos = v3(&m["pos"]);
                if m["kind"].as_str() == Some("npc") || id >= 100 {
                    seen_npc.push(id);
                    self.npcs.insert(id, Npc { pos, vel: v3(&m["vel"]), seen_t: t });
                } else {
                    let ws = m["weapons"].as_array();
                    let has = |k: &str| ws.map_or(false, |a| a.iter().any(|x| x.as_str() == Some(k)));
                    let old = self.mechs.get(&id);
                    if old.map_or(true, |tr| t - tr.seen_t > 6.0) {
                        self.new_contact = true;
                    }
                    let hit_me_t = old.map_or(-100.0, |tr| tr.hit_me_t);
                    self.mechs.insert(
                        id,
                        Track {
                            pos,
                            vel: v3(&m["vel"]),
                            aim: v2(&m["aim"]),
                            hp: fnum_or(&m["hp"], 1400.0),
                            cubes: m["cubes"].as_i64().unwrap_or(0) as i32,
                            gun: ws.is_none() || has("gun"),
                            laser: has("laser"),
                            lightning: has("lightning"),
                            mortar: has("mortar"),
                            charging: m["charging"].as_bool().unwrap_or(false),
                            seen_t: t,
                            hit_me_t,
                        },
                    );
                }
            }
        }
        self.mechs.retain(|_, tr| t - tr.seen_t < 30.0);
        // a drone we should be seeing but are not is dead
        {
            let map = &self.map;
            self.npcs.retain(|_, n| {
                if t - n.seen_t < 5.6 {
                    return true;
                }
                let d = d2(&n.pos, &me_pos);
                !(d < 38.0 && map.los(eye, [n.pos[0], n.pos[1], n.pos[2] + 2.0]))
            });
        }

        // items: remember what we saw, forget what is visibly gone
        fn sync(mem: &mut HashMap<i64, Item>, arr: &Value, t: f32, map: &Map, eye: [f32; 3]) {
            if let Some(a) = arr.as_array() {
                for it in a {
                    let id = it["id"].as_i64().unwrap_or(-1);
                    let kind = it["kind"].as_str().and_then(wk);
                    mem.insert(id, Item { pos: v3(&it["pos"]), kind, seen_t: t });
                }
            }
            mem.retain(|_, it| {
                if it.seen_t >= t {
                    return true;
                }
                let d = d2(&it.pos, &eye);
                !(d < 38.0 && map.los(eye, [it.pos[0], it.pos[1], it.pos[2] + 1.2]))
            });
        }
        sync(&mut self.crates, &o["crates"], t, &self.map, eye);
        sync(&mut self.cubes, &o["cubes"], t, &self.map, eye);
        sync(&mut self.pickups, &o["pickups"], t, &self.map, eye);

        self.shots.clear();
        if let Some(a) = o["projectiles"].as_array() {
            for pr in a {
                if matches!(pr["kind"].as_str(), Some("gun") | Some("npc")) {
                    self.shots.push((v3(&pr["pos"]), v3(&pr["vel"])));
                }
            }
        }
        self.drops.clear();
        if let Some(a) = o["drops"].as_array() {
            for d in a {
                self.drops.push(DropObs { id: d["id"].as_i64().unwrap_or(-1), pos: v3(&d["pos"]), lands_in: fnum(&d["lands_in"]), holder: d["holder"].as_i64() });
            }
        }
        if self.start_alive < 0 {
            self.start_alive = self.alive.max(2);
        }
        self.offer = o["offer"]["choices"].as_array().map(|a| a.iter().map(|c| c["id"].as_str().unwrap_or("").to_string()).collect());

        if let Some(evs) = o["events"].as_array() {
            for e in evs {
                match e["type"].as_str() {
                    Some("damaged") => {
                        if e["weapon"].as_str() == Some("zone") {
                            continue;
                        }
                        self.last_hit_t = t;
                        if let Some(b) = e["bearing"].as_f64() {
                            self.last_hit_bearing = b as f32;
                        }
                        match e["by"].as_i64() {
                            Some(id) if id < 100 => {
                                if self.kills == 0 && !self.early_attackers.contains(&id) {
                                    self.early_attackers.push(id);
                                }
                                if self.pursuer.0 == id && t - self.pursuer.2 < 6.0 {
                                    self.pursuer.2 = t;
                                } else {
                                    self.pursuer = (id, t, t);
                                }
                                self.last_mech_hit_t = t;
                                self.last_hit_by = id;
                                if let Some(tr) = self.mechs.get_mut(&id) {
                                    tr.hit_me_t = t;
                                }
                            }
                            Some(id) => {
                                self.npc_hit_t = t;
                                self.npc_hit_by = id;
                            }
                            None => {}
                        }
                    }
                    Some("hit") => {
                        if let Some(id) = e["target"].as_i64() {
                            self.my_hit_t.insert(id, t);
                        }
                    }
                    Some("kill") => {
                        self.kills += 1;
                        // Experiment D: forget the dead mech at once. Its stale track made the bot
                        // skip the cubes it dropped for 10 s and walk away from them.
                        if let Some(id) = e["target"].as_i64() {
                            self.mechs.remove(&id);
                        }
                    }
                    Some("took") | Some("take_failed") => {
                        if let Some(id) = e["id"].as_i64() {
                            if e["reason"].as_str() != Some("range") {
                                self.pickups.remove(&id);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        // other mechs losing health in gun- or laser-sized steps that we did not cause: mechs are
        // shooting each other, which the practice squad never does (drone hits are 25 each)
        if let Some(arr) = o["mechs"].as_array() {
            for m in arr {
                let id = m["id"].as_i64().unwrap_or(-1);
                if id < 0 || id >= 100 || !m["hp"].is_number() {
                    continue;
                }
                let hp = fnum(&m["hp"]);
                if let Some(&(prev, pt)) = self.hp_seen.get(&id) {
                    let drop = prev - hp;
                    let mine = self.my_hit_t.get(&id).map_or(false, |ht| t - ht < 0.6);
                    if t - pt < 0.25 && drop >= 30.0 && !mine {
                        self.busy_t.insert(id, t);
                    }
                    if t - pt < 0.25 && drop >= 44.0 && !mine {
                        let near25 = (drop - 25.0 * (drop / 25.0).round()).abs() < 2.5;
                        let two_drones_on_armor = (drop - 46.0).abs() < 1.6 || (drop - 42.0).abs() < 1.6;
                        if !near25 && !two_drones_on_armor {
                            self.hp_drops += if drop >= 100.0 { 2 } else { 1 };
                        }
                    }
                }
                self.hp_seen.insert(id, (hp, t));
            }
        }
        // weapon fire that is not a mech shooting at us
        if let Some(arr) = o["sounds"].as_array() {
            for sd in arr {
                let kind = sd["kind"].as_str().unwrap_or("");
                if !matches!(kind, "gun" | "laser" | "lightning" | "mortar" | "explosion") {
                    continue;
                }
                let b = fnum(&sd["bearing"]);
                let from_hunter = self.mechs.values().any(|tr| {
                    t - tr.seen_t < 1.5 && {
                        let to = (tr.pos[1] - me_pos[1]).atan2(tr.pos[0] - me_pos[0]);
                        let back = (me_pos[1] - tr.pos[1]).atan2(me_pos[0] - tr.pos[0]);
                        wrap(to - b).abs() < 0.45 && (t - tr.hit_me_t < 4.0 || wrap(tr.aim[0] - back).abs() < 0.4)
                    }
                });
                if !from_hunter {
                    self.snd_other += 1;
                }
            }
        }
        if !self.ffa {
            let unexplained = self.start_alive - self.kills - self.alive;
            // one early death we did not cause is enough when looking for a free-for-all quickly;
            // otherwise two, so that one stray drone kill does not flip the mode
            // (in 13 recorded team free-for-alls the first death came between 7 and 16 s)
            let need = if self.pol.ffa_fast && t < 30.0 { 1 } else { 2 };
            if unexplained >= need {
                if self.mismatch_t < 0.0 {
                    self.mismatch_t = t;
                } else if t - self.mismatch_t > 1.0 {
                    self.ffa = true;
                    self.ffa_why = "deaths";
                }
            } else {
                self.mismatch_t = -1.0;
            }
            if self.pol.ffa_fast && !self.ffa {
                // Softer signs: none is trusted alone (nothing public shows what the practice squad
                // sounds like), any two together are.
                let fighting = self.hp_drops >= 2;
                let two = self.early_attackers.len() >= 2 && self.kills == 0;
                if (t <= 8.0 && self.snd_other >= 40) || (t <= 15.0 && self.snd_other >= 75) {
                    self.snd_ffa = true;
                }
                if (fighting as u8) + (two as u8) + (self.snd_ffa as u8) >= 2 {
                    self.ffa = true;
                    self.ffa_why = "two signs";
                }
            }
        }
    }

    // ------------------------------------------------------------------ zone helpers
    fn zone_at(&self, tt: f32) -> ([f32; 2], f32) {
        let z = &self.zone;
        let s = z.s.max(self.t);
        if tt >= z.e {
            return (z.nc, z.nr);
        }
        if tt <= s || z.e <= s {
            return (z.c, z.r);
        }
        let f = ((tt - s) / (z.e - s)).clamp(0.0, 1.0);
        ([z.c[0] + (z.nc[0] - z.c[0]) * f, z.c[1] + (z.nc[1] - z.c[1]) * f], z.r + (z.nr - z.r) * f)
    }

    /// Absolute time at which `p` stops being inside the zone (infinite if it is inside the next circle).
    fn time_out(&self, p: [f32; 2], margin: f32) -> f32 {
        let inside = |tt: f32| {
            let (c, r) = self.zone_at(tt);
            d2(&p, &c) < r - margin
        };
        if !inside(self.t) {
            return self.t;
        }
        let end = self.zone.e.max(self.t);
        if inside(end + 0.01) {
            return f32::INFINITY;
        }
        let (mut lo, mut hi) = (self.t, end);
        for _ in 0..14 {
            let mid = 0.5 * (lo + hi);
            if inside(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    fn zm(&self) -> f32 {
        let nr = self.zone.nr;
        if nr < 8.0 {
            nr * 0.5
        } else {
            (0.25 * nr).clamp(4.0, 22.0)
        }
    }

    // ------------------------------------------------------------------ where to stand
    fn score_spot(&self, p: [f32; 2], evade: bool) -> f32 {
        let m = &self.map;
        let z = &self.zone;
        let t = self.t;
        let c = m.cell_of(p[0], p[1]);
        let chest = [p[0], p[1], m.surf[c] + 5.0];
        let zm = self.zm();
        let mut s = 0.0f32;
        let depth = z.nr - d2(&p, &z.nc);
        if depth < zm {
            // the zone outranks everything else: no pile of enemy penalties may outweigh it
            s -= 5000.0 + (zm - depth) * 10.0;
        } else if z.nr > 60.0 && !evade {
            let want = (0.22 * z.nr).clamp(12.0, 60.0);
            s -= (depth - want).abs() / z.nr * 18.0;
        }
        if d2(&p, &z.c) > z.r - 4.0 {
            s -= 10000.0;
        }
        s -= d2(&p, &self.me.pos) * 0.06;
        s += m.cover(p[0], p[1]) as f32 * if evade { 0.5 } else { 1.0 };
        for tr in self.mechs.values() {
            let age = t - tr.seen_t;
            if age > 20.0 {
                continue;
            }
            let w = if age < 6.0 { 1.0 } else { 0.5 };
            let d = d2(&p, &tr.pos).max(5.0);
            let mut pen = 60.0 * w / (1.0 + (d / 60.0).powi(2));
            if d < 260.0 && m.los_coarse([tr.pos[0], tr.pos[1], tr.pos[2] + 8.0], chest) {
                pen = pen * 2.0 + if evade { 45.0 * w } else { 8.0 * w };
            }
            s -= pen;
        }
        if let Some(v) = self.virt {
            if t < self.virt_until && m.los_coarse(v, chest) {
                s -= 40.0;
            }
        }
        for n in self.npcs.values() {
            let d = d2(&p, &n.pos);
            if d < 55.0 {
                s -= 12.0 + 25.0 * (1.0 - d / 55.0);
            }
        }
        for dr in &self.drops {
            if dr.lands_in < 30.0 && d2(&p, &dr.pos) < 70.0 {
                s -= 15.0;
            }
        }
        s
    }

    fn pick_spot(&mut self, evade: bool) -> Option<[f32; 2]> {
        let me_p = [self.me.pos[0], self.me.pos[1]];
        let z = self.zone.clone();
        let zm = self.zm();
        let mut cands: Vec<[f32; 2]> = Vec::with_capacity(72);
        cands.push(me_p);
        let n_local = if evade { 28 } else { 16 };
        for _ in 0..n_local {
            let a = self.rand() * TAU;
            let r = if evade { 25.0 + self.rand() * 90.0 } else { 15.0 + self.rand() * 110.0 };
            cands.push([me_p[0] + r * a.cos(), me_p[1] + r * a.sin()]);
        }
        let rs = (z.nr - zm).max(0.0);
        let base = (me_p[1] - z.nc[1]).atan2(me_p[0] - z.nc[0]);
        for _ in 0..(if evade { 6 } else { 18 }) {
            let a = base + (self.rand() - 0.5) * 3.2;
            let r = rs * (0.35 + 0.65 * self.rand().sqrt());
            cands.push([z.nc[0] + r * a.cos(), z.nc[1] + r * a.sin()]);
        }
        cands.push(z.nc);
        let mut best: Option<([f32; 2], f32)> = None;
        if let Some(h) = self.spot {
            if self.spot_evade == evade {
                let c = self.map.cell_of(h[0], h[1]);
                if !self.map.blocked[c] {
                    best = Some((h, self.score_spot(h, evade) + 4.0));
                }
            }
        }
        for p in cands {
            let c = self.map.cell_of(p[0], p[1]);
            let c = match self.map.nearest_free(c, 2, true) {
                Some(c) => c,
                None => continue,
            };
            let q = self.map.center(c);
            let s = self.score_spot(q, evade);
            if best.map_or(true, |b| s > b.1) {
                best = Some((q, s));
            }
        }
        best.map(|b| b.0)
    }

    // ------------------------------------------------------------------ what to collect
    fn choose_loot(&self, soon: bool, careful: bool) -> Option<Loot> {
        let me = &self.me;
        let t = self.t;
        // careful: do not walk into the view of a mech we know about to fetch something
        let watched = |p: &[f32; 3]| -> bool {
            careful
                && self.mechs.values().any(|tr| {
                    t - tr.seen_t < 8.0 && d2(&tr.pos, p) < 230.0 && self.map.los_coarse([tr.pos[0], tr.pos[1], tr.pos[2] + 8.0], [p[0], p[1], p[2] + 4.5])
                })
        };
        let z = &self.zone;
        let zm = self.zm();
        let hp_frac = me.hp / me.max_hp;
        let enemy_near = |p: &[f32; 3], r: f32| self.mechs.values().any(|tr| t - tr.seen_t < 10.0 && d2(&tr.pos, p) < r);
        let npc_near = |p: &[f32; 3], r: f32| self.npcs.values().any(|n| d2(&n.pos, p) < r);
        let zone_ok = |p: &[f32; 3], dist: f32| -> bool {
            let tout = self.time_out([p[0], p[1]], zm * 0.5 + 2.0);
            if tout == f32::INFINITY {
                return true;
            }
            if soon {
                return false;
            }
            let arrive = t + dist / (me.walk * 0.6) + 4.0;
            let back = (d2(p, &z.nc) - (z.nr - zm)).max(0.0) / (me.walk * 0.6) + 6.0;
            arrive + back + 5.0 < tout
        };
        let reach = |p: &[f32; 3]| -> bool {
            let c = self.map.cell_of(p[0], p[1]);
            match self.map.nearest_free(c, 2, true) {
                Some(k) => (p[2] - self.map.surf[k]).abs() < 3.5,
                None => false,
            }
        };
        let have = |k: WK| me.w.iter().any(|w| w.map_or(false, |w| w.kind == k));
        let free_slot = me.w.iter().any(|w| w.is_none());
        let mut cands: Vec<(Loot, f32)> = Vec::new();
        let upgrade_to_laser = !self.pol.lightning && !free_slot && have(WK::Lightning) && !have(WK::Laser);
        let gun_gives_way = self.pol.lightning && !free_slot && have(WK::Gun);
        if free_slot || upgrade_to_laser || gun_gives_way {
            for (id, it) in &self.pickups {
                let k = match it.kind {
                    Some(k) => k,
                    None => continue,
                };
                if k == WK::Mortar || have(k) || (upgrade_to_laser && k != WK::Laser) {
                    continue;
                }
                // arm first: a drone next to the weapon is worth its few hits
                let armf = self.pol.arm_first && free_slot;
                if enemy_near(&it.pos, if armf { 35.0 } else { 60.0 }) || (!armf && npc_near(&it.pos, 48.0)) {
                    continue;
                }
                let value = if self.pol.lightning {
                    // lightning first; the second special weapon is worth less of a detour
                    let base = if k == WK::Lightning { 260.0 } else { 220.0 };
                    if gun_gives_way { base * 0.5 } else { base }
                } else if upgrade_to_laser {
                    70.0
                } else if k == WK::Laser {
                    260.0
                } else {
                    200.0
                };
                cands.push((Loot { kind: LK::Pickup, id: *id, pos: it.pos }, value));
            }
        }
        let hide = self.pol.hide && self.ffa;
        let farm = self.pol.farm && self.ffa;
        for (id, it) in &self.cubes {
            if enemy_near(&it.pos, 45.0) {
                continue;
            }
            cands.push((Loot { kind: LK::Cube, id: *id, pos: it.pos }, if farm { 160.0 } else { 80.0 }));
        }

        for (id, it) in &self.crates {
            if hide || enemy_near(&it.pos, if farm { 50.0 } else { 70.0 }) || npc_near(&it.pos, 50.0) {
                continue;
            }
            cands.push((Loot { kind: LK::Crate, id: *id, pos: it.pos }, if farm { 90.0 } else { 55.0 }));
        }
        if !hide && hp_frac > if farm { 0.5 } else { 0.6 } && !enemy_near(&me.pos, if farm { 100.0 } else { 150.0 }) {
            for (id, n) in &self.npcs {
                if enemy_near(&n.pos, if farm { 90.0 } else { 120.0 }) {
                    continue;
                }
                cands.push((Loot { kind: LK::Npc, id: *id, pos: n.pos }, if farm { 170.0 } else { 90.0 }));
            }
        }
        for dr in &self.drops {
            if dr.lands_in > 22.0 || enemy_near(&dr.pos, 90.0) || d2(&dr.pos, &me.pos) > 170.0 {
                continue;
            }
            if dr.holder.is_some() && dr.holder != Some(-1) && d2(&dr.pos, &me.pos) > 10.0 {
                continue;
            }
            cands.push((Loot { kind: LK::Drop, id: dr.id, pos: dr.pos }, 220.0));
        }
        let mut best: Option<(Loot, f32)> = None;
        let mut cur: Option<f32> = None;
        for (l, value) in cands {
            let d = d2(&l.pos, &me.pos);
            let arm = self.pol.arm_first && free_slot && l.kind == LK::Pickup;
            if d > if arm { 380.0 } else { 260.0 } || !reach(&l.pos) || !zone_ok(&l.pos, d) || (l.kind != LK::Npc && watched(&l.pos)) {
                continue;
            }
            // arm first: with an empty slot a weapon on the ground outranks every crate, cube and
            // drone, however much nearer they are (a second weapon about doubles our damage)
            let s = if arm { 1000.0 - d + if self.pol.laser_pref && self.pickups.get(&l.id).map_or(false, |it| it.kind == Some(WK::Laser)) { 120.0 } else { 0.0 } } else { value / (d * 1.3 + 25.0) };
            if let Some(c) = &self.loot {
                if c.kind == l.kind && c.id == l.id {
                    cur = Some(s);
                }
            }
            if best.map_or(true, |b| s > b.1) {
                best = Some((l, s));
            }
        }
        match (best, cur, self.loot) {
            (Some((_, bs)), Some(cs), Some(c)) if bs < cs * 1.6 => {
                // keep the current target; refresh its position
                let pos = match c.kind {
                    LK::Npc => self.npcs.get(&c.id).map(|n| n.pos).unwrap_or(c.pos),
                    _ => c.pos,
                };
                Some(Loot { kind: c.kind, id: c.id, pos })
            }
            (Some((l, _)), _, _) => Some(l),
            _ => None,
        }
    }

    // ------------------------------------------------------------------ movement
    fn go_to(&mut self, a: &mut Act, goal: [f32; 2], stop_r: f32, avoid: bool) {
        let x = self.me.pos[0];
        let y = self.me.pos[1];
        let dg = d2(&[x, y], &goal);
        if dg < stop_r {
            a.mv = [0.0, 0.0];
            return;
        }
        // progress watch: three seconds without getting 2 m closer to the same goal means the route
        // the grid believes in does not work here
        if d2(&self.prog_goal, &goal) > 4.0 {
            self.prog_goal = goal;
            self.prog_best = dg;
            self.prog_since = self.dcount;
        } else if dg < self.prog_best - 2.0 {
            self.prog_best = dg;
            self.prog_since = self.dcount;
        } else if self.dcount - self.prog_since >= 30 && dg > 12.0 {
            self.noprog = true;
            self.prog_since = self.dcount;
            self.prog_best = dg;
        }
        let need = self.path.is_empty() || self.dcount - self.path_tick >= 3 || d2(&self.path_goal, &goal) > 4.0;
        // time guard: a search costs up to about 2 ms; if this decision has already been slow
        // (a busy scorer), keep following the path we have rather than risk the 50 ms budget
        let late = self.tick_start.elapsed().as_millis() >= 12 && !self.path.is_empty();
        if need && !late {
            let sc = self.map.nearest_free(self.map.cell_of(x, y), 5, false);
            let on_main = sc.map_or(false, |c| self.map.comp[c] == self.map.main_comp);
            let gc = self.map.nearest_free(self.map.cell_of(goal[0], goal[1]), 12, on_main);
            self.path.clear();
            self.path_found = false;
            if let (Some(sc), Some(gc)) = (sc, gc) {
                let minx = self.map.minx;
                let miny = self.map.miny;
                let gx = self.map.gx;
                let npcs: Vec<[f32; 2]> = if avoid {
                    let skip = self.loot.filter(|l| l.kind == LK::Npc).map(|l| l.id);
                    self.npcs.iter().filter(|(id, _)| Some(**id) != skip).map(|(_, n)| [n.pos[0], n.pos[1]]).collect()
                } else {
                    Vec::new()
                };
                let t = self.t;
                let ens: Vec<[f32; 2]> = if avoid { self.mechs.values().filter(|tr| t - tr.seen_t < 10.0).map(|tr| [tr.pos[0], tr.pos[1]]).collect() } else { Vec::new() };
                // keep paths inside the circle as it will be 20 s from now
                let (zc, zr) = self.zone_at(self.t + 20.0);
                // extra path cost on a coarse 16 m grid (cheap to look up inside A*)
                let cgx = gx / 8 + 1;
                let cgy = self.map.gy / 8 + 1;
                let mut danger = vec![0u32; cgx * cgy];
                for qj in 0..cgy {
                    for qi in 0..cgx {
                        let cx = minx + 16.0 * qi as f32 + 8.0;
                        let cy = miny + 16.0 * qj as f32 + 8.0;
                        let mut e = 0u32;
                        for n in &npcs {
                            if (cx - n[0]).powi(2) + (cy - n[1]).powi(2) < 2700.0 {
                                e += 500;
                            }
                        }
                        for m in &ens {
                            if (cx - m[0]).powi(2) + (cy - m[1]).powi(2) < 4900.0 {
                                e += 300;
                            }
                        }
                        // outside the zone is close to forbidden; the last few metres inside are dear
                        let dz = ((cx - zc[0]).powi(2) + (cy - zc[1]).powi(2)).sqrt();
                        if dz > zr - 2.0 {
                            e += 3000;
                        } else if dz > zr - 18.0 {
                            e += 250;
                        }
                        danger[qj * cgx + qi] = e;
                    }
                }
                let extra = move |c: usize| -> u32 { danger[(c / gx / 8) * cgx + (c % gx) / 8] };
                let t0 = std::time::Instant::now();
                let (p, found) = self.pl.astar(&self.map, sc, gc, 9_000, extra);
                self.prof[3] = self.prof[3].max(t0.elapsed().as_secs_f32() * 1000.0);
                self.path = p;
                self.path_found = found;
            }
            self.path_goal = goal;
            self.path_tick = self.dcount;
        }
        let mut target = goal;
        if self.path.len() >= 2 {
            let mut i0 = 0usize;
            let mut bd = f32::INFINITY;
            for (i, &c) in self.path.iter().enumerate().take(48) {
                let p = self.map.center(c as usize);
                let d = d2(&p, &[x, y]);
                if d < bd {
                    bd = d;
                    i0 = i;
                }
            }
            let last = self.path.len() - 1;
            let mut k = (i0 + 12).min(last);
            loop {
                let p = self.map.center(self.path[k] as usize);
                if k <= i0 + 1 || self.map.walk_clear(x, y, p[0], p[1]) {
                    target = p;
                    break;
                }
                k -= 1;
            }
            if k == last && self.path_found && (dg < 4.0 || self.map.walk_clear(x, y, goal[0], goal[1])) {
                target = goal;
            }
            if !self.path_found && self.pol.climb {
                // The goal cannot be walked to (a walled bowl, a courtyard, a roof). At the end of
                // what can be walked, press straight toward it; the climb routine does the rest.
                let end = self.map.center(self.path[last] as usize);
                if d2(&end, &[x, y]) < 5.0 {
                    target = goal;
                }
            }
        }
        let dx = target[0] - x;
        let dy = target[1] - y;
        let d = (dx * dx + dy * dy).sqrt();
        if d > 0.05 {
            a.mv = [dx / d, dy / d];
        }
    }

    /// Is `q` out of sight of a shooter whose muzzle is at `from` (body and head both hidden)?
    fn hidden_from(&self, q: [f32; 2], from: [f32; 3]) -> bool {
        let g = self.map.ground(q[0], q[1]);
        !self.map.los(from, [q[0], q[1], g + 4.5]) && !self.map.los(from, [q[0], q[1], g + 8.6])
    }

    /// The nearest spot, by walking distance, that the given shooters cannot see. A flood over
    /// the nav grid out to 90 m of walking; only cells next to an obstacle are tested (cover is
    /// always next to something), and the first hidden one found is the closest.
    fn find_cover(&mut self, shooters: &[[f32; 3]]) -> Option<[f32; 2]> {
        let me_p = [self.me.pos[0], self.me.pos[1]];
        let start = self.map.nearest_free(self.map.cell_of(me_p[0], me_p[1]), 5, false)?;
        // the four nearest shooters decide
        let mut sh: Vec<[f32; 3]> = shooters.to_vec();
        sh.sort_by(|a, b| d2(a, &me_p).partial_cmp(&d2(b, &me_p)).unwrap_or(std::cmp::Ordering::Equal));
        sh.truncate(4);
        let m = &self.map;
        let z = &self.zone;
        let gx = m.gx as isize;
        let gy = m.gy as isize;
        if self.cover_epoch == u32::MAX {
            self.cover_seen.iter_mut().for_each(|x| *x = 0);
            self.cover_epoch = 0;
        }
        self.cover_epoch += 1;
        let ep = self.cover_epoch;
        let mut queue: std::collections::VecDeque<(u32, u16)> = std::collections::VecDeque::with_capacity(4096);
        self.cover_seen[start] = ep;
        queue.push_back((start as u32, 0));
        let mut best: Option<(f32, [f32; 2])> = None;
        let mut best_depth = u16::MAX;
        while let Some((c32, depth)) = queue.pop_front() {
            let c = c32 as usize;
            // a few more rings after the first find, to pick the better of near-equal spots
            if depth > 45 || depth > best_depth.saturating_add(4) {
                break;
            }
            let (ci, cj) = ((c % m.gx) as isize, (c / m.gx) as isize);
            if m.wall_near[c] <= 2 && (ci + cj) % 2 == 0 && depth >= 2 {
                let q = m.center(c);
                let g = m.surf[c];
                let inside = d2(&q, &z.c) < z.r - 8.0;
                if inside && sh.iter().all(|s0| !m.los_coarse(*s0, [q[0], q[1], g + 4.5]) && !m.los_coarse(*s0, [q[0], q[1], g + 8.6])) {
                    // closer is better; a spot walled in on more sides and away from drones breaks ties
                    let mut cost = depth as f32 * 2.0 - m.cover(q[0], q[1]) as f32 * 1.2;
                    for np in self.npcs.values() {
                        if d2(&q, &np.pos) < 50.0 {
                            cost += 14.0;
                        }
                    }
                    if best.map_or(true, |b| cost < b.0) {
                        best = Some((cost, q));
                    }
                    best_depth = best_depth.min(depth);
                }
            }
            for (k, (dx, dy)) in [(1isize, 0isize), (-1, 0), (0, 1), (0, -1)].into_iter().enumerate() {
                let (ni, nj) = (ci + dx, cj + dy);
                if ni < 0 || nj < 0 || ni >= gx || nj >= gy {
                    continue;
                }
                let n = nj as usize * m.gx + ni as usize;
                if self.cover_seen[n] == ep || !m.passable(n) || !m.can_step(c, k) {
                    continue;
                }
                self.cover_seen[n] = ep;
                queue.push_back((n as u32, depth + 1));
            }
        }
        best.map(|b| b.1)
    }

    /// Bullets already in the air fly straight, so where each will be is known. If the move we
    /// are about to make gets us hit, pick the move (eight directions or standing still) that
    /// leaves the nearest bullet farthest away. Only bullets more than a tenth of a second out can
    /// be stepped around; at close range there is nothing to gain.
    fn dodge(&self, mv: [f32; 2]) -> Option<[f32; 2]> {
        let me = &self.me;
        let rad = 2.5 * (1.0 + 0.05 * me.cubes as f32) + 0.8;
        let v0 = [me.vel[0], me.vel[1]];
        let mut inc: Vec<(f32, [f32; 2])> = Vec::new();
        for (p, v) in &self.shots {
            let r = [p[0] - me.pos[0], p[1] - me.pos[1]];
            let vv = v[0] * v[0] + v[1] * v[1];
            if vv < 100.0 {
                continue;
            }
            let ts = -(r[0] * v[0] + r[1] * v[1]) / vv;
            if !(0.1..=0.9).contains(&ts) {
                continue;
            }
            let bz = p[2] + v[2] * ts;
            if bz < me.pos[2] - 1.5 || bz > me.pos[2] + me.height + 1.5 {
                continue;
            }
            // would it pass anywhere near us at all?
            let at = [p[0] + v[0] * ts, p[1] + v[1] * ts];
            if d2(&at, &me.pos) > 14.0 {
                continue;
            }
            inc.push((ts, at));
        }
        if inc.is_empty() {
            return None;
        }
        let miss = |c: [f32; 2]| -> f32 {
            let vt = [c[0] * me.walk, c[1] * me.walk];
            let dv = [vt[0] - v0[0], vt[1] - v0[1]];
            let dn = (dv[0] * dv[0] + dv[1] * dv[1]).sqrt();
            let tacc = dn / 58.5;
            let mut worst = f32::INFINITY;
            for (ts, at) in &inc {
                let ta = ts.min(tacc);
                let k = if dn > 0.01 { 0.5 * 58.5 * ta * ta / dn } else { 0.0 };
                let mut q = [me.pos[0] + v0[0] * ta + dv[0] * k, me.pos[1] + v0[1] * ta + dv[1] * k];
                if *ts > tacc {
                    q[0] += vt[0] * (ts - tacc);
                    q[1] += vt[1] * (ts - tacc);
                }
                worst = worst.min(d2(&q, at));
            }
            worst
        };
        let base = miss(mv);
        if base >= rad {
            return None;
        }
        let mut best = (base + 0.6, mv);
        let x = me.pos[0];
        let y = me.pos[1];
        for k in 0..9 {
            let c = if k == 8 { [0.0, 0.0] } else { let a = k as f32 * TAU / 8.0; [a.cos(), a.sin()] };
            if k < 8 {
                let (px, py) = (x + c[0] * 5.0, y + c[1] * 5.0);
                if !self.map.passable(self.map.cell_of(px, py)) || !self.map.walk_clear(x, y, px, py) || d2(&[px, py], &self.zone.c) > self.zone.r - 4.0 {
                    continue;
                }
            }
            let m = miss(c).min(rad + 2.0) + 0.2 * (c[0] * mv[0] + c[1] * mv[1]);
            if m > best.0 {
                best = (m, c);
            }
        }
        if best.1 == mv {
            None
        } else {
            Some(best.1)
        }
    }

    // ------------------------------------------------------------------ roofs
    /// A place to climb roof `r` from: a free ground spot a few metres outside one of its edges
    /// with nothing but that roof's own wall in between, and the landing spot just inside the edge.
    fn roof_access(&self, r: usize, from: [f32; 2]) -> Option<([f32; 2], [f32; 2])> {
        let m = &self.map;
        let rf = &m.roofs[r];
        let mut best: Option<(f32, [f32; 2], [f32; 2])> = None;
        for k in 0..32 {
            let ang = k as f32 * TAU / 32.0;
            let (sn, cs) = ang.sin_cos();
            let mut d = 0.0f32;
            let mut edge = None;
            while d < 160.0 {
                match m.roof_at(rf.center[0] + cs * d, rf.center[1] + sn * d) {
                    Some((rr, _)) if rr == r => {}
                    _ => {
                        edge = Some(d - 1.0);
                        break;
                    }
                }
                d += 1.0;
            }
            let de = match edge {
                Some(e) if e > 5.0 => e,
                _ => continue,
            };
            let land = [rf.center[0] + cs * (de - 4.0), rf.center[1] + sn * (de - 4.0)];
            if m.roof_at(land[0], land[1]).map(|x| x.0) != Some(r) {
                continue;
            }
            for out in [5.0f32, 6.5, 8.0] {
                let f = [rf.center[0] + cs * (de + out), rf.center[1] + sn * (de + out)];
                let cell = m.cell_of(f[0], f[1]);
                if !m.passable(cell) || m.comp[cell] != m.main_comp {
                    continue;
                }
                let climb = rf.top - m.ground(f[0], f[1]);
                if !(8.0..=48.0).contains(&climb) {
                    continue;
                }
                // only this roof's own wall between the foot and the edge
                let mut clean = true;
                let mut q = de + out - 1.0;
                while q > de + 1.5 {
                    if m.is_solid(rf.center[0] + cs * q, rf.center[1] + sn * q) {
                        clean = false;
                        break;
                    }
                    q -= 1.0;
                }
                if !clean {
                    continue;
                }
                let cost = d2(&f, &from);
                if best.map_or(true, |b| cost < b.0) {
                    best = Some((cost, f, land));
                }
                break;
            }
        }
        best.map(|b| (b.1, b.2))
    }

    /// The best roof to fight from right now: near, deep, tall enough, and well inside the next circle.
    fn pick_roof(&self) -> Option<(usize, [f32; 2], [f32; 2])> {
        let me_p = [self.me.pos[0], self.me.pos[1]];
        let z = &self.zone;
        let zm = self.zm();
        let m = &self.map;
        let t = self.t;
        let mut cands: Vec<(f32, usize)> = Vec::new();
        for (i, rf) in m.roofs.iter().enumerate() {
            if self.roof_bad.contains(&i) {
                continue;
            }
            let c = rf.center;
            if c[0] < m.bx0 + 20.0 || c[0] > m.bx1 - 20.0 || c[1] < m.by0 + 20.0 || c[1] > m.by1 - 20.0 {
                continue;
            }
            let d = d2(&c, &me_p);
            // in a free-for-all only a roof close by is worth the run in the open
            if d > if self.ffa { 110.0 } else { 260.0 } {
                continue;
            }
            let depth_next = z.nr - d2(&c, &z.nc);
            if depth_next < zm + 12.0 || d2(&c, &z.c) > z.r - 20.0 {
                continue;
            }
            let h = rf.top - rf.ground;
            let mut s = -d + rf.depth.min(25.0) * 2.0 + (h.min(25.0) - 12.0) * 1.5 + depth_next.min(120.0) * 0.25;
            for tr in self.mechs.values() {
                if t - tr.seen_t < 10.0 && d2(&tr.pos, &c) < 60.0 {
                    s -= 40.0;
                }
            }
            cands.push((s, i));
        }
        cands.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        for (_, i) in cands.iter().take(4) {
            if let Some((foot, land)) = self.roof_access(*i, me_p) {
                return Some((*i, foot, land));
            }
        }
        None
    }

    /// Step toward a point on our roof without getting closer than `min_depth` to an edge.
    fn roof_move(&self, a: &mut Act, to: [f32; 2], min_depth: f32) {
        let x = self.me.pos[0];
        let y = self.me.pos[1];
        let d = d2(&to, &[x, y]);
        if d < 1.0 {
            a.mv = [0.0, 0.0];
            return;
        }
        let u = [(to[0] - x) / d, (to[1] - y) / d];
        for sdist in [1.5f32, 3.0] {
            match self.map.roof_at(x + u[0] * sdist, y + u[1] * sdist) {
                Some((r, dep)) if r == self.roof && dep >= min_depth => {}
                _ => {
                    a.mv = [0.0, 0.0];
                    return;
                }
            }
        }
        let k = (d / 3.0).min(1.0).max(0.4);
        a.mv = [u[0] * k, u[1] * k];
    }

    /// The spot on our roof from which to look down at `tp`: toward it, 3 m short of the edge.
    fn roof_peek(&self, tp: [f32; 3]) -> [f32; 2] {
        let x = self.me.pos[0];
        let y = self.me.pos[1];
        let d = d2(&tp, &[x, y]).max(0.01);
        let u = [(tp[0] - x) / d, (tp[1] - y) / d];
        let mut best = [x, y];
        let mut sdist = 1.0;
        while sdist < d.min(120.0) {
            let p = [x + u[0] * sdist, y + u[1] * sdist];
            match self.map.roof_at(p[0], p[1]) {
                Some((r, dep)) if r == self.roof && dep >= 3.0 => best = p,
                _ => break,
            }
            sdist += 1.0;
        }
        best
    }

    /// Strafe on the roof across the line to the target, never nearer than `keep` to an edge.
    fn roof_jink(&mut self, a: &mut Act, tp: [f32; 3], keep: f32) {
        let x = self.me.pos[0];
        let y = self.me.pos[1];
        let d = d2(&tp, &[x, y]).max(0.01);
        let u = [(tp[0] - x) / d, (tp[1] - y) / d];
        if self.t > self.strafe_until {
            self.strafe_dir = if self.rand() < 0.5 { 1.0 } else { -1.0 };
            self.strafe_until = self.t + 0.35 + self.rand() * 0.45;
        }
        for sd in [self.strafe_dir, -self.strafe_dir] {
            let v = [-sd * u[1], sd * u[0]];
            let ok = [2.0f32, 4.5].iter().all(|sdist| matches!(self.map.roof_at(x + v[0] * sdist, y + v[1] * sdist), Some((r, dep)) if r == self.roof && dep >= keep));
            if ok {
                a.mv = v;
                self.strafe_dir = sd;
                return;
            }
        }
        a.mv = [0.0, 0.0];
    }

    /// Direction in which to walk off our roof: toward `goal` if that edge drops to the ground.
    fn roof_exit_dir(&self, goal: [f32; 2]) -> [f32; 2] {
        let x = self.me.pos[0];
        let y = self.me.pos[1];
        let top = self.map.roofs[self.roof].top;
        let base = (goal[1] - y).atan2(goal[0] - x);
        for off in [0.0f32, 0.4, -0.4, 0.8, -0.8, 1.2, -1.2, 1.6, -1.6, 2.2, -2.2, PI] {
            let ang = base + off;
            let u = [ang.cos(), ang.sin()];
            // walk to the edge, then make sure the far side is lower and stays lower
            let mut sdist = 1.0;
            let mut edge = None;
            while sdist < 200.0 {
                match self.map.roof_at(x + u[0] * sdist, y + u[1] * sdist) {
                    Some((r, _)) if r == self.roof => {}
                    _ => {
                        edge = Some(sdist);
                        break;
                    }
                }
                sdist += 1.0;
            }
            if let Some(e) = edge {
                let clear = [1.0f32, 3.0, 5.0, 7.0].iter().all(|q| self.map.support(x + u[0] * (e + q), y + u[1] * (e + q)) < top - 3.0);
                if clear {
                    return u;
                }
            }
        }
        [base.cos(), base.sin()]
    }

    fn fight_move(&mut self, a: &mut Act, tp: [f32; 3], want: f32) {
        let x = self.me.pos[0];
        let y = self.me.pos[1];
        let dx = tp[0] - x;
        let dy = tp[1] - y;
        let d = (dx * dx + dy * dy).sqrt().max(0.01);
        let u = [dx / d, dy / d];
        if self.t > self.strafe_until {
            self.strafe_dir = if self.rand() < 0.5 { 1.0 } else { -1.0 };
            self.strafe_until = self.t + 0.35 + self.rand() * 0.45;
        }
        let radial = if d > want + 8.0 {
            0.8
        } else if d < want - 8.0 {
            -0.8
        } else {
            0.0
        };
        let sd0 = self.strafe_dir;
        let mut chosen: Option<([f32; 2], f32)> = None;
        for sd in [sd0, -sd0] {
            let vx = radial * u[0] - sd * u[1];
            let vy = radial * u[1] + sd * u[0];
            let n = (vx * vx + vy * vy).sqrt().max(0.01);
            let v = [vx / n, vy / n];
            let px = x + v[0] * 7.0;
            let py = y + v[1] * 7.0;
            let inside = d2(&[px, py], &self.zone.c) < self.zone.r - 6.0;
            if inside && self.map.passable(self.map.cell_of(px, py)) && self.map.walk_clear(x, y, px, py) {
                chosen = Some((v, sd));
                break;
            }
        }
        match chosen {
            Some((v, sd)) => {
                a.mv = v;
                self.strafe_dir = sd;
            }
            None => {
                a.mv = [radial * u[0], radial * u[1]];
            }
        }
    }

    // ------------------------------------------------------------------ aim and fire
    fn angles(from: &[f32; 3], to: &[f32; 3]) -> [f32; 2] {
        let dx = to[0] - from[0];
        let dy = to[1] - from[1];
        let dz = to[2] - from[2];
        let h = (dx * dx + dy * dy).sqrt().max(0.001);
        [dy.atan2(dx), dz.atan2(h)]
    }

    fn aim_fire(&mut self, a: &mut Act, tg: &Tgt, allow_laser: bool, precharge: bool) {
        let me = self.me.clone();
        let muzzle = [me.pos[0], me.pos[1], me.pos[2] + me.muzzle_h];
        let tc = [tg.pos[0], tg.pos[1], tg.pos[2] + tg.h * 0.5];
        let lat = 0.075f32;
        let at = |tt: f32| -> [f32; 3] { [tc[0] + tg.vel[0] * tt, tc[1] + tg.vel[1] * tt, tc[2]] };
        let direct_p = at(lat);
        let direct = Brain::angles(&muzzle, &direct_p);
        let dist = d3(&muzzle, &tc);
        let los = self.map.los(muzzle, tc);
        let tol_y = (tg.r * 0.85).atan2(dist.max(1.0)) + 0.012;
        let tol_p = (tg.h * 0.42).atan2(dist.max(1.0)) + 0.012;
        let aligned = |want: &[f32; 2]| -> bool { wrap(want[0] - me.aim[0]).abs() < tol_y && (want[1] - me.aim[1]).abs() < tol_p };
        let mut aim = direct;
        let mut laser_now = false;
        for i in 0..2 {
            let w = match me.w[i] {
                Some(w) => w,
                None => continue,
            };
            match w.kind {
                WK::Laser => {
                    if w.charge > 0.0 {
                        if w.charge >= w.charge_t - 0.11 {
                            laser_now = true;
                            let ok = los && aligned(&direct) && dist < w.range - 3.0;
                            // let go when on target; otherwise keep holding for a while
                            a.fire[i] = !(ok || w.charge > w.charge_t + 1.4);
                        } else {
                            a.fire[i] = true;
                        }
                    } else if allow_laser && (los || precharge) && dist < w.range - 8.0 && w.cooldown <= 0.12 {
                        a.fire[i] = true;
                    }
                }
                WK::Lightning => {
                    let half = (w.cone * 0.5).to_radians() * 0.8;
                    if los && dist < w.range * 0.94 && wrap(direct[0] - me.aim[0]).abs() < half && me.energy > 10.0 {
                        a.fire[i] = true;
                    }
                }
                _ => {}
            }
        }
        for i in 0..2 {
            let w = match me.w[i] {
                Some(w) if w.kind == WK::Gun => w,
                _ => continue,
            };
            let mut tt = dist / w.speed;
            for _ in 0..3 {
                let p = at(tt + lat);
                tt = (d3(&muzzle, &p) - 3.0).max(0.0) / w.speed;
            }
            let p = at(tt + lat);
            let ga = Brain::angles(&muzzle, &p);
            let in_range = d3(&muzzle, &p) < w.range - 3.0;
            if !laser_now {
                aim = ga;
            }
            if tg.mech {
                // gate statistics: [decisions with a mech target, no line of sight, out of range, not aligned, reloading, fired]
                self.gate[0] += 1;
                if !los { self.gate[1] += 1; } else if !in_range { self.gate[2] += 1; } else if w.reload > 0.0 { self.gate[4] += 1; } else if !aligned(&ga) { self.gate[3] += 1; } else { self.gate[5] += 1; }
            }
            if los && in_range && aligned(&ga) && w.reload <= 0.0 {
                a.fire[i] = true;
            }
            if w.ammo == 0 && w.reload <= 0.0 {
                a.reload = true;
            }
        }
        a.aim = aim;
    }

    // ------------------------------------------------------------------ the decision
    pub fn decide(&mut self, o: &Value) -> Value {
        let t_start = std::time::Instant::now();
        self.tick_start = t_start;
        self.ingest(o);
        self.prof[0] = self.prof[0].max(t_start.elapsed().as_secs_f32() * 1000.0);
        self.dcount += 1;
        let me = self.me.clone();
        let t = self.t;
        let me_p = [me.pos[0], me.pos[1]];
        let eye = [me.pos[0], me.pos[1], me.pos[2] + me.eye_h];
        let mut a = Act { mv: [0.0, 0.0], boost: false, jump: false, aim: self.cmd_aim, fire: [false, false], reload: false, pick: None, take: None, drop: None };

        // upgrades: best-ranked choice
        if let Some(ch) = &self.offer {
            let mut best = (usize::MAX, 0usize);
            for (i, id) in ch.iter().enumerate() {
                let order: &[&str] = if self.pol.lightning { &LIGHTNING_ORDER } else if self.pol.ffa_tough && self.ffa { &FFA_ORDER } else { &UPGRADE_ORDER };
                let rank = order.iter().position(|u| *u == id.as_str()).unwrap_or(100);
                if rank < best.0 {
                    best = (rank, i);
                }
            }
            a.pick = Some(best.1 as i64);
        }
        // weapons on the ground within reach
        let have = |k: WK| me.w.iter().any(|w| w.map_or(false, |w| w.kind == k));
        if me.w.iter().any(|w| w.is_none()) {
            for (id, it) in &self.pickups {
                if let Some(k) = it.kind {
                    if k != WK::Mortar && !have(k) && d2(&it.pos, &me.pos) < self.pickup_r - 0.4 && (it.pos[2] - me.pos[2]).abs() < 3.7 {
                        a.take = Some(*id);
                        break;
                    }
                }
            }
        } else if self.pol.lightning {
            // lightning doctrine: lightning and laser are the pair to hold; the gun gives way
            if let Some(gslot) = me.w.iter().position(|w| w.map_or(false, |w| w.kind == WK::Gun)) {
                for (id, it) in &self.pickups {
                    if let Some(k) = it.kind {
                        if (k == WK::Lightning || k == WK::Laser) && !have(k) && d2(&it.pos, &me.pos) < self.pickup_r - 0.4 && (it.pos[2] - me.pos[2]).abs() < 3.7 {
                            a.take = Some(*id);
                            a.drop = Some(gslot as i64);
                            break;
                        }
                    }
                }
            }
        } else if have(WK::Lightning) && !have(WK::Laser) {
            // a laser outranges everything: swap the lightning for it
            let slot = me.w.iter().position(|w| w.map_or(false, |w| w.kind == WK::Lightning)).unwrap_or(1);
            for (id, it) in &self.pickups {
                if it.kind == Some(WK::Laser) && d2(&it.pos, &me.pos) < self.pickup_r - 0.4 && (it.pos[2] - me.pos[2]).abs() < 3.7 {
                    a.take = Some(*id);
                    a.drop = Some(slot as i64);
                    break;
                }
            }
        }

        // ---- zone timing
        let z = self.zone.clone();
        let zm = self.zm();
        let d_safe = (d2(&me_p, &z.nc) - (z.nr - zm)).max(0.0);
        let t_out = self.time_out(me_p, zm * 0.5);
        let need = d_safe / (me.walk * 0.6) + 6.0;
        // once the zone has us moving, keep going until we are properly inside the next circle
        // (version 1 dithered on its edge between "safe" and "late" and took zone damage)
        if !me.in_zone || (t_out - t) < need {
            self.zone_latch = true;
        } else if d_safe <= 0.0 {
            self.zone_latch = false;
        }
        let urgent = self.zone_latch && (d_safe > 0.0 || !me.in_zone);
        let soon = urgent || (t_out - t) < need + 15.0;

        // ---- threats
        let mut ths: Vec<Th> = Vec::new();
        for (id, tr) in &self.mechs {
            let age = t - tr.seen_t;
            if age > 0.35 {
                continue;
            }
            let d = d2(&tr.pos, &me.pos);
            let los = self.map.los(eye, [tr.pos[0], tr.pos[1], tr.pos[2] + 4.5]);
            let bearing = (me.pos[1] - tr.pos[1]).atan2(me.pos[0] - tr.pos[0]);
            let facing = wrap(tr.aim[0] - bearing).abs() < 0.35;
            let hostile = t - tr.hit_me_t < 4.0;
            ths.push(Th { id: *id, pos: tr.pos, vel: tr.vel, d, los, facing, hostile, hp: tr.hp, dps: dps_of(tr.gun, tr.laser, tr.lightning, tr.mortar, d, tr.cubes), cubes: tr.cubes, lightning: tr.lightning });
        }
        let combat: Vec<Th> = ths.iter().filter(|h| h.los && (h.hostile || (h.facing && h.d < 250.0) || h.d < 55.0)).cloned().collect();
        let mut primary: Option<Th> = None;
        for h in &combat {
            let better = match &primary {
                None => true,
                Some(p) => (h.hostile && !p.hostile) || (h.hostile == p.hostile && h.d < p.d),
            };
            if better {
                primary = Some(*h);
            }
        }
        // shot by someone we cannot see: remember roughly where from
        if t - self.last_mech_hit_t < 0.15 && !ths.iter().any(|h| h.id == self.last_hit_by && h.los) {
            let b = self.last_hit_bearing;
            self.virt = Some([me.pos[0] + 110.0 * b.cos(), me.pos[1] + 110.0 * b.sin(), me.pos[2] + 8.0]);
            self.virt_until = t + 6.0;
            self.evade_until = self.evade_until.max(t + 3.0);
        }

        // ---- fight or break contact
        let my_gun = have(WK::Gun);
        let my_laser = have(WK::Laser);
        let my_light = have(WK::Lightning) && me.energy > 20.0;
        let hpf = me.hp / me.max_hp;
        if self.regen_mode && hpf > 0.85 {
            self.regen_mode = false;
        }
        let armed = my_laser || my_light;
        let mut fight = false;
        let mut evade = t < self.evade_until;
        if let Some(p) = &primary {
            let myd = dps_of(my_gun, my_laser, my_light, false, p.d, me.cubes);
            let thd: f32 = combat.iter().map(|h| h.dps).sum();
            let ttk_them = p.hp / myd.max(1.0);
            let ttk_me = me.hp / thd.max(1.0);
            let endgame = self.alive <= 3 || z.r < 60.0;
            if self.ffa {
                // free-for-all: only take fights we should win
                fight = ttk_them < ttk_me * 0.85 || p.hp < 300.0 || (endgame && ttk_them < ttk_me * 1.3);
                if self.pol.stand && combat.len() == 1 && p.hostile && hpf > 0.35 && ttk_them < ttk_me * 1.25 {
                    fight = true;
                }
                if t < self.fight_until && hpf > 0.3 {
                    fight = true;
                }
                if hpf < 0.3 && ttk_them > 2.0 && !endgame {
                    fight = false;
                }
                // one mech that has kept after us for 10 s will not be shaken off: turn and fight
                // it while we still have the health to (this also keeps a wrongly raised
                // free-for-all flag from turning a practice hunter into an endless chase)
                if self.pursuer.0 == p.id && t - self.pursuer.1 > 10.0 && t - self.pursuer.2 < 4.0 && combat.len() == 1 && hpf > 0.35 {
                    fight = true;
                }
            } else if self.pol.opening_caution && t < 20.0 && p.d > 70.0 && p.hp > 300.0 {
                // first seconds, mode still unknown: a shot from afar is a free-for-all opener far
                // more often than a practice hunter. Do not trade in the open; get out of sight.
                fight = false;
            } else {
                // practice ladder: every hunter has to be killed; back off only to regenerate
                let losing = ttk_me < ttk_them * 0.8;
                if !endgame && ((hpf < 0.4 && losing && p.hp > 250.0) || (combat.len() >= 3 && hpf < 0.6)) {
                    self.regen_mode = true;
                }
                // outnumbered: break off earlier, while there is still health to get away with
                if self.pol.backoff && !endgame && p.hp > 250.0 && ((combat.len() >= 3 && hpf < 0.8) || (combat.len() == 2 && hpf < 0.5 && losing)) {
                    self.regen_mode = true;
                }
                // losing a duel: leave it sooner, with enough health left to reach cover
                let v = self.pol.backoff2;
                if v > 0 && v < 4 && !endgame && p.hp > 250.0 && losing {
                    let (one, two) = match v { 1 => (0.55, 0.5), 2 => (0.4, 0.65), _ => (0.55, 0.65) };
                    if (combat.len() == 1 && hpf < one) || (combat.len() == 2 && hpf < two) {
                        self.regen_mode = true;
                    }
                }
                // three or more at once is never a fight to stand in
                if v == 4 && !endgame && combat.len() >= 3 && p.hp > 250.0 {
                    self.regen_mode = true;
                }
                fight = !self.regen_mode || p.hp < 200.0 || p.d < 35.0;
            }
            if fight {
                if t >= self.fight_until {
                    self.fight_until = t + 2.0;
                }
                evade = false;
            } else {
                evade = true;
                // once out of sight, stay there a while instead of stepping straight back out
                self.evade_until = self.evade_until.max(t + if self.ffa || (self.pol.opening_caution && t < 20.0) { 5.0 } else { 2.0 });
            }
        } else if !evade && !self.regen_mode {
            // a lone mech in view that is not looking at us: open on its back when the odds are good
            let vis: Vec<&Th> = ths.iter().filter(|h| h.los && h.d < 200.0).collect();
            if self.pol.vulture && self.ffa && hpf > 0.55 && !vis.iter().any(|h| h.facing) {
                // free-for-all: a mech busy trading with someone else is the cheapest kill there is;
                // shoot first while it is looking the other way
                let mut best: Option<(f32, Th)> = None;
                for h in &vis {
                    let busy = self.busy_t.get(&h.id).map_or(false, |bt| t - bt < 2.5);
                    if busy && h.d > 25.0 && h.d < 190.0 && h.hp < me.hp * 1.1 {
                        let sc = h.hp + 2.0 * h.d;
                        if best.map_or(true, |b| sc < b.0) {
                            best = Some((sc, **h));
                        }
                    }
                }
                if let Some((_, p)) = best {
                    primary = Some(p);
                    fight = true;
                    self.fight_until = t + 2.0;
                }
            }
            if !fight && vis.len() == 1 {
                let p = vis[0];
                let myd = dps_of(my_gun, my_laser, my_light, false, p.d, me.cubes) * 1.4;
                let good = if self.ffa { hpf > 0.7 && p.hp / myd < 0.45 * (me.hp / p.dps.max(1.0)) && p.hp < 0.6 * me.hp } else { hpf > 0.8 && armed && p.d < 170.0 };
                if good {
                    primary = Some(*p);
                    fight = true;
                    self.fight_until = t + 2.0;
                }
            }
        }
        if self.regen_mode && !fight {
            evade = true;
        }
        // finish wounded targets: a hunter we hurt that slips out of the fight is back at full
        // health soon after, so keep after it while we are the healthier one and nobody is on us
        let mut pursue: Option<[f32; 2]> = None;
        if self.pol.finish && !self.ffa && !fight && !evade && !urgent && primary.is_none() && t - self.last_fight_t < 12.0 && hpf > 0.5 {
            if let Some(tr) = self.mechs.get(&self.last_tgt) {
                let d = d2(&tr.pos, &me.pos);
                if t - tr.seen_t < 8.0 && tr.hp < 650.0 && me.hp > tr.hp + 200.0 && d < 220.0 && d2(&tr.pos, &z.nc) < z.nr - 6.0 {
                    if let Some(h) = ths.iter().find(|h| h.id == self.last_tgt && h.los) {
                        primary = Some(*h);
                        fight = true;
                        self.fight_until = t + 2.0;
                    } else {
                        pursue = Some([tr.pos[0], tr.pos[1]]);
                    }
                }
            }
        }
        // free-for-all (or the first seconds, when we cannot tell yet): whoever can see us will
        // shoot within a second or two; be gone first
        let careful = self.ffa || (self.pol.opening_caution && t < 20.0);
        let mut watchers: Vec<[f32; 3]> = Vec::new();
        if careful && self.pol.unseen && !fight {
            for h in ths.iter().filter(|h| h.los && h.d < 220.0) {
                watchers.push([h.pos[0], h.pos[1], h.pos[2] + 7.0]);
            }
            if !watchers.is_empty() {
                evade = true;
            }
        }

        // ---- roof bookkeeping (practice squad only)
        let roof_ok = (self.pol.roof && !self.ffa) || (self.pol.roof_ffa && self.ffa);
        let roof_passive = self.ffa;
        let on_roof_now: Option<(usize, f32)> = match self.map.roof_at(me.pos[0], me.pos[1]) {
            Some((r, dep)) if (me.pos[2] - self.map.roofs[r].top).abs() < 1.5 => Some((r, dep)),
            _ => None,
        };
        match self.roof_st {
            RoofSt::On => {
                // still up there? (being in the air just above it counts)
                let above = self.map.roof_at(me.pos[0], me.pos[1]).map(|x| x.0) == Some(self.roof) && me.pos[2] > self.map.roofs[self.roof].top - 1.0;
                if !above || !roof_ok {
                    self.roof_st = RoofSt::Off;
                }
            }
            RoofSt::Leave => {
                if (on_roof_now.is_none() && me.grounded) || t - self.roof_t0 > 8.0 {
                    self.roof_st = RoofSt::Off;
                }
            }
            RoofSt::Approach | RoofSt::Climb => {
                if !roof_ok {
                    self.roof_st = RoofSt::Off;
                }
            }
            RoofSt::Off => {}
        }
        // choose a roof to fight from
        if roof_ok && self.roof_st == RoofSt::Off && t >= self.roof_ban && t - self.roof_pick_t >= 3.0 && !urgent {
            self.roof_pick_t = t;
            if let Some((r, foot, land)) = self.pick_roof() {
                self.roof = r;
                self.roof_foot = foot;
                self.roof_land = land;
                self.roof_st = RoofSt::Approach;
                self.roof_t0 = t;
            }
        }
        if self.roof_st == RoofSt::Approach {
            // give up on a roof the zone is about to leave behind, or that takes too long to reach
            let c = self.map.roofs[self.roof].center;
            if z.nr - d2(&c, &z.nc) < zm + 6.0 || t - self.roof_t0 > 60.0 {
                self.roof_st = RoofSt::Off;
                self.roof_ban = t + 5.0;
            }
        }
        let mut roof_tgt: Option<Th> = None;
        let mut roof_hidden = false;
        // a weapon we still need lying close by on the ground is worth a detour before going up
        let weapon_near = me.w.iter().any(|w| w.is_none())
            && self.pickups.values().any(|it| {
                matches!(it.kind, Some(k) if k != WK::Mortar && !have(k)) && d2(&it.pos, &me.pos) < 90.0 && (it.pos[2] - self.map.ground(it.pos[0], it.pos[1])).abs() < 3.0
            });

        // ---- movement goal
        let mut label = "hold";
        let mut resting = false;
        let mut loot: Option<Loot> = None;
        let nc_changed = d2(&self.spot_nc, &z.nc) > 1.0 || !self.spot_nc[0].is_finite();
        let want_spot = |b: &mut Brain, ev: bool| -> Option<[f32; 2]> {
            if b.spot.is_none() || b.spot_evade != ev || nc_changed || b.new_contact || b.dcount - b.spot_tick >= if ev { 5 } else { 10 } {
                let t0 = std::time::Instant::now();
                let s = b.pick_spot(ev);
                b.prof[1] = b.prof[1].max(t0.elapsed().as_secs_f32() * 1000.0);
                b.spot = s;
                b.spot_evade = ev;
                b.spot_tick = b.dcount;
                b.spot_nc = b.zone.nc;
                b.new_contact = false;
            }
            b.spot
        };
        if self.roof_st == RoofSt::Leave {
            // walking off the roof: keep going until we are down
            label = "down";
            self.loot = None;
            a.mv = self.roof_dir;
            // held up by something on the way to the edge: hop, and try a little to one side
            let sp = (me.vel[0] * me.vel[0] + me.vel[1] * me.vel[1]).sqrt();
            if sp < 1.0 && me.grounded {
                self.roof_still += 1;
            } else {
                self.roof_still = 0;
            }
            if self.roof_still >= 8 {
                self.roof_still = 0;
                a.jump = true;
                let (sn, cs) = 0.8f32.sin_cos();
                self.roof_dir = [self.roof_dir[0] * cs - self.roof_dir[1] * sn, self.roof_dir[0] * sn + self.roof_dir[1] * cs];
            }
        } else if urgent {
            label = "zone";
            self.loot = None;
            let g = if z.nr < 10.0 { Some(z.nc) } else { want_spot(self, false) };
            let g = g.unwrap_or(z.nc);
            if self.roof_st == RoofSt::On {
                // the zone calls: off the roof on the side of the next circle
                self.roof_dir = self.roof_exit_dir(g);
                self.roof_st = RoofSt::Leave;
                self.roof_t0 = t;
                a.mv = self.roof_dir;
            } else {
                self.roof_st = RoofSt::Off;
                self.go_to(&mut a, g, 1.5, false);
            }
            a.boost = me.energy > 8.0 && me.grounded && (!me.in_zone || (t_out - t) < need * 0.6);
        } else if self.roof_st == RoofSt::Climb {
            // committed: up the wall and onto the roof
            label = "climb";
            self.loot = None;
            let rf = self.map.roofs[self.roof].clone();
            let to = self.roof_land;
            let dd = d2(&to, &me_p).max(0.01);
            a.mv = [(to[0] - me_p[0]) / dd, (to[1] - me_p[1]) / dd];
            if me.grounded {
                a.jump = true;
            } else {
                a.boost = me.pos[2] < rf.top + 1.2 && me.energy > 0.5;
            }
            if on_roof_now.map(|x| x.0) == Some(self.roof) && me.grounded {
                self.roof_st = RoofSt::On;
                self.roof_t0 = t;
                self.roof_fails = 0;
                self.patrol_t = t;
            } else if t - self.roof_t0 > 9.0 || (me.grounded && t - self.roof_t0 > 1.5 && me.energy < 6.0) {
                // did not make it: try once more, then never this roof again
                self.roof_fails += 1;
                self.roof_st = RoofSt::Off;
                self.roof_ban = t + 6.0;
                if self.roof_fails >= 2 {
                    self.roof_bad.push(self.roof);
                    self.roof_fails = 0;
                }
            }
        } else if self.roof_st == RoofSt::On {
            label = "roof";
            self.loot = None;
            let rf = self.map.roofs[self.roof].clone();
            let my_depth = on_roof_now.map(|x| x.1).unwrap_or(0.0);
            let roof = self.roof;
            let on_it = |h: &Th| self.map.roof_at(h.pos[0], h.pos[1]).map(|x| x.0) == Some(roof) && (h.pos[2] - rf.top).abs() < 3.0;
            let up: Vec<Th> = ths.iter().filter(|h| on_it(h)).cloned().collect();
            let key = |h: &Th| h.hp + 3.0 * h.d;
            // free-for-all: only a mech that is shooting us is a target; nobody is looked for
            let vis = ths.iter().filter(|h| h.los && h.d < 235.0 && (!roof_passive || h.hostile)).cloned().min_by(|x, y| key(x).partial_cmp(&key(y)).unwrap_or(std::cmp::Ordering::Equal));
            let trk = ths.iter().filter(|h| !h.los && h.d < 170.0).cloned().min_by(|x, y| x.d.partial_cmp(&y.d).unwrap_or(std::cmp::Ordering::Equal));
            let mut ready = false;
            for w in me.w.iter().flatten() {
                match w.kind {
                    WK::Gun => ready |= w.reload <= 0.7,
                    WK::Laser => ready |= w.cooldown <= 0.0 && w.charge >= w.charge_t - 0.5,
                    WK::Lightning => ready |= vis.map_or(false, |h| h.d < w.range) && me.energy > 15.0,
                    _ => {}
                }
            }
            if up.len() >= 2 || (!up.is_empty() && hpf < 0.35) {
                // they came up after us and we are losing: off the far side
                let e = up[0].pos;
                let goal = [me_p[0] + (me_p[0] - e[0]) * 3.0, me_p[1] + (me_p[1] - e[1]) * 3.0];
                self.roof_dir = self.roof_exit_dir(goal);
                self.roof_st = RoofSt::Leave;
                self.roof_t0 = t;
                self.roof_ban = t + 25.0;
                a.mv = self.roof_dir;
            } else if let Some(h) = vis {
                roof_tgt = Some(h);
                if up.is_empty() && (hpf < 0.4 || (!ready && my_depth < 9.0)) {
                    // hurt, or reloading at the edge: step back out of sight
                    self.roof_move(&mut a, rf.center, 3.0);
                } else {
                    self.roof_jink(&mut a, h.pos, if my_depth < 6.0 { 3.0 } else { 5.0 });
                }
            } else if trk.is_some() && ready && hpf > 0.5 && !roof_passive {
                // it is under the edge: step out and look down at it
                let h = trk.unwrap();
                roof_tgt = Some(h);
                roof_hidden = true;
                let pk = self.roof_peek(h.pos);
                self.roof_move(&mut a, pk, 3.0);
            } else {
                // nobody to shoot: rest in the middle, and look over an edge every so often
                if roof_passive {
                    self.patrol_t = t - 5.0; // never walk to an edge to look
                }
                if hpf > 0.8 && t - self.patrol_t > 10.0 {
                    self.patrol_k += 1;
                    self.patrol_t = t;
                }
                if hpf > 0.8 && t - self.patrol_t < 4.5 {
                    let ang = self.patrol_k as f32 * 2.4;
                    let pk = self.roof_peek([me_p[0] + 300.0 * ang.cos(), me_p[1] + 300.0 * ang.sin(), 0.0]);
                    self.roof_move(&mut a, pk, 3.0);
                } else {
                    self.roof_move(&mut a, rf.center, 3.0);
                }
            }
        } else if self.roof_st == RoofSt::Approach && (fight || evade || !weapon_near) {
            // on the way to our roof (shooting back as we go); a needed weapon lying about is taken first
            label = "toroof";
            self.loot = None;
            let foot = self.roof_foot;
            let rf_top = self.map.roofs[self.roof].top;
            if d2(&foot, &me_p) < 1.8 {
                if me.grounded && me.energy >= (rf_top - me.pos[2]) * 1.6 + 15.0 {
                    self.roof_st = RoofSt::Climb;
                    self.roof_t0 = t;
                }
                a.mv = [0.0, 0.0];
            } else {
                self.go_to(&mut a, foot, 1.2, !(fight || evade));
                a.boost = (fight || evade) && me.energy > 60.0 && d2(&foot, &me_p) > 25.0;
            }
        } else if fight {
            label = "fight";
            self.loot = None;
            let p = primary.unwrap();
            self.last_tgt = p.id;
            self.last_fight_t = t;
            let want = if my_light && (!my_laser || self.pol.lightning) {
                28.0
            } else if my_laser {
                125.0
            } else if p.lightning {
                95.0
            } else {
                85.0
            };
            // unarmed and a weapon is near: fight on the way to it rather than on the spot
            let mut fetch: Option<[f32; 2]> = None;
            if self.pol.arm_first && me.w.iter().any(|w| w.is_none()) && hpf > 0.35 {
                let mut bd = 150.0f32;
                for it in self.pickups.values() {
                    let d = d2(&it.pos, &me.pos);
                    let ground = (it.pos[2] - self.map.ground(it.pos[0], it.pos[1])).abs() < 3.0;
                    let inside = d2(&it.pos, &z.c) < z.r - 6.0;
                    if matches!(it.kind, Some(k) if k != WK::Mortar && !have(k)) && d < bd && ground && inside && self.map.passable(self.map.cell_of(it.pos[0], it.pos[1])) {
                        bd = d;
                        fetch = Some([it.pos[0], it.pos[1]]);
                    }
                }
            }
            // one hunter at a time: with two or more on us, stand where the target can be shot
            // and as few of the others as possible can see us
            let mut split: Option<[f32; 2]> = None;
            if self.pol.split && !self.ffa && combat.len() >= 2 && fetch.is_none() {
                if self.dcount - self.split_tick >= 5 {
                    let others: Vec<[f32; 3]> = combat.iter().filter(|h| h.id != p.id).map(|h| [h.pos[0], h.pos[1], h.pos[2] + 7.0]).collect();
                    let chest = [p.pos[0], p.pos[1], p.pos[2] + 4.5];
                    let seen_at = |b: &Brain, q: [f32; 2]| -> usize { others.iter().filter(|o| !b.hidden_from(q, **o)).count() };
                    let here = seen_at(self, me_p);
                    let mut best: Option<(f32, [f32; 2], usize)> = None;
                    for r in [6.0f32, 12.0, 18.0] {
                        for k in 0..12 {
                            let ang = k as f32 * TAU / 12.0 + r;
                            let c = self.map.cell_of(me_p[0] + r * ang.cos(), me_p[1] + r * ang.sin());
                            if !self.map.passable(c) {
                                continue;
                            }
                            let q = self.map.center(c);
                            if d2(&q, &z.c) > z.r - 6.0 || !self.map.los([q[0], q[1], self.map.surf[c] + me.muzzle_h], chest) {
                                continue;
                            }
                            let n = seen_at(self, q);
                            let clear = self.map.walk_clear(me_p[0], me_p[1], q[0], q[1]);
                            if !clear {
                                continue;
                            }
                            let sc = -(n as f32) * 30.0 - r * if clear { 0.3 } else { 0.6 } - (d2(&q, &p.pos) - want).abs() * 0.05;
                            if best.map_or(true, |b| sc > b.0) {
                                best = Some((sc, q, n));
                            }
                        }
                    }
                    self.split_spot = best.filter(|b| b.2 < here).map(|b| b.1);
                    self.split_tick = self.dcount;
                }
                split = self.split_spot.filter(|q| d2(q, &me_p) > 2.5);
            }
            match (fetch, split) {
                (Some(g), _) => self.go_to(&mut a, g, 1.0, false),
                (None, Some(g)) => self.go_to(&mut a, g, 1.5, false),
                (None, None) => self.fight_move(&mut a, p.pos, want),
            }
        } else if evade {
            label = "evade";
            self.loot = None;
            let exposed = combat.iter().any(|h| h.los) || t - self.last_mech_hit_t < 2.0 || !watchers.is_empty();
            let mut g: Option<[f32; 2]> = None;
            // (practice fights after the opening keep the scored retreat spot they were measured with)
            if self.pol.near_cover && careful {
                // who to hide from: the mechs engaging us, and whoever shot us from out of view
                let mut shooters: Vec<[f32; 3]> = combat.iter().map(|h| [h.pos[0], h.pos[1], h.pos[2] + 7.0]).collect();
                if shooters.is_empty() {
                    for h in ths.iter().filter(|h| h.hostile) {
                        shooters.push([h.pos[0], h.pos[1], h.pos[2] + 7.0]);
                    }
                }
                if let Some(v) = self.virt {
                    if t < self.virt_until && shooters.is_empty() {
                        shooters.push(v);
                    }
                }
                for w in &watchers {
                    if !shooters.iter().any(|sh| d2(sh, w) < 3.0) {
                        shooters.push(*w);
                    }
                }
                // when staying unseen matters, hide from every mech we know of, not only the ones
                // looking at us now: running from one into the view of another gains nothing
                if careful && !shooters.is_empty() {
                    for tr in self.mechs.values() {
                        let o = [tr.pos[0], tr.pos[1], tr.pos[2] + 7.0];
                        if t - tr.seen_t < 10.0 && d2(&o, &me.pos) < 250.0 && !shooters.iter().any(|sh| d2(sh, &o) < 3.0) {
                            shooters.push(o);
                        }
                    }
                }
                if !shooters.is_empty() {
                    // keep the spot we are running to while it still hides us; look again otherwise
                    let keep = self.cover.filter(|c| self.dcount - self.cover_tick < 40 && shooters.iter().all(|sh| self.hidden_from(*c, *sh)));
                    if keep.is_none() {
                        self.cover = self.find_cover(&shooters);
                        self.cover_tick = self.dcount;
                    }
                    g = self.cover;
                }
            }
            let g = match g {
                Some(g) => g,
                // nobody has us in sight any more: stay behind the cover we ran to
                None if self.pol.near_cover && careful && self.cover.map_or(false, |c| self.dcount - self.cover_tick < 80 && d2(&c, &me_p) < 4.0) => me_p,
                None => want_spot(self, true).unwrap_or(z.nc),
            };
            self.go_to(&mut a, g, 1.5, false);
            a.boost = exposed && me.energy > 12.0;
        } else if let Some(pp) = pursue {
            label = "chase";
            self.loot = None;
            self.go_to(&mut a, pp, 25.0, false);
        } else {
            let t0 = std::time::Instant::now();
            loot = self.choose_loot(soon, careful);
            self.prof[2] = self.prof[2].max(t0.elapsed().as_secs_f32() * 1000.0);
            // heal between hunters: hurt after a fight, with nobody on us, stay hidden until the
            // regeneration has done its work; only a weapon or cube a few steps away is worth leaving for
            if ((self.pol.heal && !self.ffa) || (self.pol.ffa_heal && self.ffa)) && hpf < 0.85 && t - self.last_mech_hit_t < 45.0 {
                if loot.map_or(false, |l| d2(&l.pos, &me.pos) > 30.0 || matches!(l.kind, LK::Npc | LK::Crate)) {
                    loot = None;
                }
                resting = loot.is_none();
            }
            self.loot = loot;
            if resting {
                label = "rest";
                let g = want_spot(self, true).unwrap_or(me_p);
                self.go_to(&mut a, g, 2.0, true);
            } else if let Some(l) = loot {
                label = "loot";
                let d = d2(&l.pos, &me.pos);
                let los = self.map.los(eye, [l.pos[0], l.pos[1], l.pos[2] + 1.5]);
                match l.kind {
                    LK::Npc => {
                        if d < 42.0 && los {
                            self.fight_move(&mut a, l.pos, 30.0);
                        } else {
                            self.go_to(&mut a, [l.pos[0], l.pos[1]], 20.0, true);
                        }
                    }
                    LK::Crate => self.go_to(&mut a, [l.pos[0], l.pos[1]], 9.0, true),
                    LK::Drop => self.go_to(&mut a, [l.pos[0], l.pos[1]], 2.5, true),
                    _ => self.go_to(&mut a, [l.pos[0], l.pos[1]], 1.0, true),
                }
            } else {
                let g = if z.nr < 10.0 { Some(z.nc) } else { want_spot(self, false) };
                if let Some(g) = g {
                    self.go_to(&mut a, g, 2.0, true);
                }
            }
        }

        // ---- hidden play: walk below the step-sound speed while nothing presses
        if self.pol.hide && self.ffa && !soon && matches!(label, "hold" | "loot" | "rest") && t - self.last_hit_t > 6.0 {
            let n = (a.mv[0] * a.mv[0] + a.mv[1] * a.mv[1]).sqrt();
            let k = 4.6 / me.walk.max(1.0);
            if n > k {
                a.mv = [a.mv[0] / n * k, a.mv[1] / n * k];
                a.boost = false;
            }
        }

        // ---- step out of the way of bullets in the air
        if self.pol.dodge && me.grounded && !urgent && matches!(self.roof_st, RoofSt::Off | RoofSt::Approach) {
            if let Some(c) = self.dodge(a.mv) {
                a.mv = c;
                self.dodges += 1;
            }
        }

        // ---- aim and fire
        let mut tgt: Option<Tgt> = None;
        let mut allow_laser = false;
        if let Some(h) = &roof_tgt {
            let k = 1.0 + 0.05 * h.cubes as f32;
            tgt = Some(Tgt { pos: h.pos, vel: h.vel, r: 2.5 * k, h: 9.0 * k, mech: true });
            allow_laser = true;
        } else if let Some(p) = &primary {
            if fight || p.hostile || p.facing {
                let k = 1.0 + 0.05 * p.cubes as f32;
                tgt = Some(Tgt { pos: p.pos, vel: p.vel, r: 2.5 * k, h: 9.0 * k, mech: true });
                allow_laser = fight;
            }
        }
        if tgt.is_none() {
            // a drone that is shooting at us
            if t - self.npc_hit_t < 3.0 {
                if let Some(n) = self.npcs.get(&self.npc_hit_by) {
                    if t - n.seen_t < 0.35 && d2(&n.pos, &me.pos) < 90.0 {
                        tgt = Some(Tgt { pos: n.pos, vel: n.vel, r: 1.8, h: 3.5, mech: false });
                        allow_laser = true;
                    }
                }
            }
        }
        if tgt.is_none() {
            if let Some(l) = loot {
                let d = d2(&l.pos, &me.pos);
                match l.kind {
                    LK::Npc if d < 75.0 => {
                        if let Some(n) = self.npcs.get(&l.id) {
                            if t - n.seen_t < 0.35 {
                                tgt = Some(Tgt { pos: n.pos, vel: n.vel, r: 1.8, h: 3.5, mech: false });
                                allow_laser = true;
                            }
                        }
                    }
                    LK::Crate if d < 48.0 => {
                        if self.crates.get(&l.id).map_or(false, |c| t - c.seen_t < 0.35) {
                            tgt = Some(Tgt { pos: l.pos, vel: [0.0; 3], r: 1.4, h: 2.4, mech: false });
                            allow_laser = true;
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(tg) = tgt {
            self.aim_fire(&mut a, &tg, allow_laser, roof_hidden);
        } else {
            // never leave a laser charging with nothing to shoot
            let mut look: Option<f32> = None;
            let mut bd = f32::INFINITY;
            for tr in self.mechs.values() {
                let d = d2(&tr.pos, &me.pos);
                if t - tr.seen_t < 8.0 && d < 250.0 && d < bd {
                    bd = d;
                    look = Some((tr.pos[1] - me.pos[1]).atan2(tr.pos[0] - me.pos[0]));
                }
            }
            if look.is_none() && t - self.last_hit_t < 3.0 {
                look = Some(self.last_hit_bearing);
            }
            match look {
                Some(b) if self.dcount % 40 < 26 => a.aim = [b, 0.0],
                _ => a.aim = [wrap(me.aim[0] + 1.2), 0.0],
            }
            for i in 0..2 {
                if let Some(w) = me.w[i] {
                    if w.kind == WK::Gun && w.ammo < w.mag - 3 && w.reload <= 0.0 && !ths.iter().any(|h| h.los) {
                        a.reload = true;
                    }
                }
            }
        }

        if self.pol.climb && !matches!(self.roof_st, RoofSt::Climb | RoofSt::Leave) {
            // ---- stuck recovery: climb over whatever holds us (jump, then jet until the feet clear it)
            let speed = (me.vel[0] * me.vel[0] + me.vel[1] * me.vel[1]).sqrt();
            let mvn = (a.mv[0] * a.mv[0] + a.mv[1] * a.mv[1]).sqrt();
            let moving = mvn > 0.5;
            if moving && speed < 1.5 && me.grounded {
                self.stuck += 1;
            } else if speed > 3.0 || !moving {
                self.stuck = 0;
            }
            if self.jet_wait > 0 {
                self.jet_wait -= 1;
            }
            // a jump costs nothing and a short jet very little: start at once unless the last try ran dry
            let jet_min = if self.jet_fails > 0 { 85.0 } else { 5.0 };
            if self.stuck >= 3 && me.grounded {
                a.boost = false; // running into a wall only burns the energy the climb needs
            }
            let noprog = self.noprog;
            self.noprog = false;
            if (self.stuck >= 6 || noprog) && self.jet == 0 && self.jet_wait == 0 && me.energy >= jet_min && moving {
                self.jet = 70;
                self.jet_from = me_p;
                // pressed against something: keep the direction we were walking. No progress along a
                // route: go straight for the goal instead, over whatever is in the way.
                let mut dir = [a.mv[0] / mvn, a.mv[1] / mvn];
                if noprog && self.stuck < 6 {
                    let g = self.prog_goal;
                    let dn = d2(&g, &me_p).max(0.01);
                    dir = [(g[0] - me_p[0]) / dn, (g[1] - me_p[1]) / dn];
                }
                // after a failed climb try a little to one side, then the other
                let rot = match self.jet_fails % 3 {
                    1 => 0.7f32,
                    2 => -0.7,
                    _ => 0.0,
                };
                let (sn, cs) = rot.sin_cos();
                self.jet_dir = [dir[0] * cs - dir[1] * sn, dir[0] * sn + dir[1] * cs];
                self.path.clear();
            }
            if self.jet > 0 {
                self.jet -= 1;
                let dir = self.jet_dir;
                a.mv = dir;
                // highest surface in the next few metres, as wide as the mech: the feet must get above it
                let mut top = f32::NEG_INFINITY;
                for sdist in [0.0f32, 1.5, 3.0, 4.5, 6.0, 7.5] {
                    for lat in [-2.6f32, 0.0, 2.6] {
                        let px = me.pos[0] + dir[0] * sdist - dir[1] * lat;
                        let py = me.pos[1] + dir[1] * sdist + dir[0] * lat;
                        top = top.max(self.map.support(px, py));
                    }
                }
                let need_z = top + 1.0;
                if me.grounded {
                    a.boost = false;
                    // something to get over, or simply not moving: hop
                    if me.pos[2] < need_z - 0.5 || speed < 1.5 {
                        a.jump = true;
                    }
                } else {
                    a.boost = me.energy > 1.0 && me.pos[2] < need_z;
                }
                let moved = d2(&me_p, &self.jet_from);
                if moved > 7.0 && me.grounded {
                    self.jet = 0;
                    self.stuck = 0;
                    self.jet_fails = 0;
                } else if (me.energy < 2.0 && me.grounded) || self.jet == 0 {
                    // did not make it: wait for the energy to come back, then try again from a full tank
                    self.jet = 0;
                    self.stuck = 0;
                    self.jet_fails += 1;
                    self.jet_wait = 20;
                }
            }
        } else if !self.pol.climb {
            // ---- stuck recovery: hop and jet over whatever holds us
            let speed = (me.vel[0] * me.vel[0] + me.vel[1] * me.vel[1]).sqrt();
            let moving = a.mv[0].abs() + a.mv[1].abs() > 0.5;
            if moving && speed < 1.2 && me.grounded {
                self.stuck += 1;
            } else if speed > 3.0 || !moving {
                self.stuck = 0;
            }
            if self.stuck >= 6 && self.unstick == 0 {
                self.unstick = 10;
                self.unstick_dir = (self.rand() - 0.5) * 2.4;
                self.path.clear();
            }
            if self.unstick > 0 {
                self.unstick -= 1;
                let ang = a.mv[1].atan2(a.mv[0]) + self.unstick_dir;
                if moving {
                    a.mv = [ang.cos(), ang.sin()];
                }
                if me.grounded {
                    a.jump = true;
                } else if me.energy > 10.0 {
                    a.boost = true;
                }
                if self.unstick == 0 {
                    self.stuck = 0;
                }
            }
        }

        if std::env::var_os("MECH_DEBUG").is_some() {
            self.dbg = format!("d_safe={:.0} t_out={:.1} urgent={} mv=({:.2},{:.2}) boost={} jump={} roof={:?}#{} foot=({:.0},{:.0}) fails={}", d_safe, t_out - t, urgent, a.mv[0], a.mv[1], a.boost, a.jump, self.roof_st, self.roof, self.roof_foot[0], self.roof_foot[1], self.roof_fails);
        }
        self.known_pickups = self.pickups.values().filter(|it| matches!(it.kind, Some(WK::Laser) | Some(WK::Lightning))).count();
        self.label = label;
        self.prof[4] = self.prof[4].max(t_start.elapsed().as_secs_f32() * 1000.0);
        a.aim[0] = wrap(a.aim[0]);
        a.aim[1] = if a.aim[1].is_finite() { a.aim[1].clamp(me.min_pitch, me.max_pitch) } else { 0.0 };
        self.cmd_aim = a.aim;
        let mut out = json!({
            "tick": self.tick.clone(),
            "move": [r4(a.mv[0]), r4(a.mv[1])],
            "boost": a.boost,
            "jump": a.jump,
            "aim": [r4(a.aim[0]), r4(a.aim[1])],
            "fire": [a.fire[0], a.fire[1]],
            "reload": a.reload,
        });
        if let Some(p) = a.pick {
            out["pick"] = json!(p);
        }
        if let Some(id) = a.take {
            out["take"] = json!(id);
        }
        if let Some(sl) = a.drop {
            out["drop"] = json!(sl);
        }
        out
    }
}
