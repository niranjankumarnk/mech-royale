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
    /// Its laser is charging right now.
    charging: bool,
}

#[derive(Clone, Copy)]
struct Tgt {
    pos: [f32; 3],
    vel: [f32; 3],
    r: f32,
    h: f32,
}

/// Where to stand in a fight: a spot that sees the target and, when there is one, a spot a few
/// steps away that the target cannot see (used while the gun reloads and the laser recharges).
#[derive(Clone, Copy)]
struct FightPlan {
    peek: [f32; 2],
    hide: Option<[f32; 2]>,
    has_los: bool,
    tick: i64,
    target: i64,
    tpos: [f32; 2],
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

/// Experiment switch: fight from a spot with a covered step next to it, reload behind cover and
/// step out of the way of a charging laser. Off = strafe in the open as in version 1.
const COVER_STEPS: bool = true;

const UPGRADE_ORDER: [&str; 22] = [
    "armor",
    "plating",
    "nanites",
    "servos",
    "laser_damage",
    "gun_damage",
    "lightning_efficiency",
    "gun_rate",
    "laser_charge",
    "reactor",
    "capacitor",
    "jets",
    "gyros",
    "gun_mag",
    "sensors",
    "gun_velocity",
    "lightning_chain",
    "lightning_range",
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
    loot: Option<Loot>,
    stuck: i32,
    unstick: i32,
    unstick_dir: f32,
    strafe_dir: f32,
    strafe_until: f32,
    fight_until: f32,
    evade_until: f32,
    dcount: i64,
    /// False while every death so far was our own kill: the practice format (one squad of house
    /// bots that never fight each other). True once somebody else has killed somebody.
    pub ffa: bool,
    kills: i32,
    start_alive: i32,
    mismatch_t: f32,
    regen_mode: bool,
    plan: Option<FightPlan>,
    last_tgt: i64,
    zone_latch: bool,
    pub label: &'static str,
    /// Debug text for the simulator trace (goal, zone timing).
    pub dbg: String,
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
            loot: None,
            stuck: 0,
            unstick: 0,
            unstick_dir: 0.0,
            strafe_dir: 1.0,
            strafe_until: 0.0,
            fight_until: -1.0,
            evade_until: -1.0,
            dcount: 0,
            ffa: false,
            kills: 0,
            start_alive: -1,
            mismatch_t: -1.0,
            regen_mode: false,
            plan: None,
            last_tgt: -1,
            zone_latch: false,
            label: "",
            prof: [0.0; 5],
            dbg: String::new(),
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
                    Some("kill") => {
                        self.kills += 1;
                        // the dead mech's track must not keep us away from the cubes it dropped
                        if let Some(id) = e["target"].as_i64() {
                            self.mechs.remove(&id);
                        }
                        self.plan = None;
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
        // a death we did not cause means a free-for-all (kill events can lag the count by a tick)
        if !self.ffa {
            // two unexplained deaths, so that one stray drone kill does not flip the mode
            if self.alive + 1 < self.start_alive - self.kills {
                if self.mismatch_t < 0.0 {
                    self.mismatch_t = t;
                } else if t - self.mismatch_t > 1.0 {
                    self.ffa = true;
                }
            } else {
                self.mismatch_t = -1.0;
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
    fn choose_loot(&self, soon: bool) -> Option<Loot> {
        let me = &self.me;
        let t = self.t;
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
        let upgrade_to_laser = !free_slot && have(WK::Lightning) && !have(WK::Laser);
        if free_slot || upgrade_to_laser {
            for (id, it) in &self.pickups {
                let k = match it.kind {
                    Some(k) => k,
                    None => continue,
                };
                if k == WK::Mortar || have(k) || (upgrade_to_laser && k != WK::Laser) {
                    continue;
                }
                if enemy_near(&it.pos, 60.0) || npc_near(&it.pos, 48.0) {
                    continue;
                }
                let value = if upgrade_to_laser { 70.0 } else if k == WK::Laser { 260.0 } else { 200.0 };
                cands.push((Loot { kind: LK::Pickup, id: *id, pos: it.pos }, value));
            }
        }
        for (id, it) in &self.cubes {
            if enemy_near(&it.pos, 45.0) {
                continue;
            }
            cands.push((Loot { kind: LK::Cube, id: *id, pos: it.pos }, 80.0));
        }
        for (id, it) in &self.crates {
            if enemy_near(&it.pos, 70.0) || npc_near(&it.pos, 50.0) {
                continue;
            }
            cands.push((Loot { kind: LK::Crate, id: *id, pos: it.pos }, 55.0));
        }
        if hp_frac > 0.6 && !enemy_near(&me.pos, 150.0) {
            for (id, n) in &self.npcs {
                if enemy_near(&n.pos, 120.0) {
                    continue;
                }
                cands.push((Loot { kind: LK::Npc, id: *id, pos: n.pos }, 90.0));
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
            if d > 260.0 || !reach(&l.pos) || !zone_ok(&l.pos, d) {
                continue;
            }
            let s = value / (d * 1.3 + 25.0);
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
        if dg < 22.0 && self.map.walk_clear(x, y, goal[0], goal[1]) {
            // short and clear: walk straight, no search
            a.mv = [(goal[0] - x) / dg.max(0.01), (goal[1] - y) / dg.max(0.01)];
            self.path.clear();
            return;
        }
        let need = self.path.is_empty() || self.dcount - self.path_tick >= 3 || d2(&self.path_goal, &goal) > 4.0;
        if need {
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
        }
        let dx = target[0] - x;
        let dy = target[1] - y;
        let d = (dx * dx + dy * dy).sqrt();
        if d > 0.05 {
            a.mv = [dx / d, dy / d];
        }
    }

    /// Strafe across the line to the target with random reversals, closing or opening the range
    /// toward `want`. With `keep_los` the next few metres must still see the target.
    fn fight_move(&mut self, a: &mut Act, tp: [f32; 3], want: f32, keep_los: bool) {
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
        let chest = [tp[0], tp[1], tp[2] + 4.5];
        let mut chosen: Option<([f32; 2], f32)> = None;
        // preferred: range correction plus strafe; then strafe only; then range correction only
        for (rad, sd) in [(radial, sd0), (radial, -sd0), (0.0, sd0), (0.0, -sd0), (radial, 0.0)] {
            if rad == 0.0 && sd == 0.0 {
                continue;
            }
            let vx = rad * u[0] - sd * u[1];
            let vy = rad * u[1] + sd * u[0];
            let n = (vx * vx + vy * vy).sqrt().max(0.01);
            let v = [vx / n, vy / n];
            let px = x + v[0] * 6.0;
            let py = y + v[1] * 6.0;
            let inside = d2(&[px, py], &self.zone.c) < self.zone.r - 6.0;
            if !inside || !self.map.passable(self.map.cell_of(px, py)) || !self.map.walk_clear(x, y, px, py) {
                continue;
            }
            if keep_los && !self.map.los([px, py, self.map.ground(px, py) + self.me.muzzle_h], chest) {
                continue;
            }
            chosen = Some((v, if sd == 0.0 { sd0 } else { sd }));
            break;
        }
        match chosen {
            Some((v, sd)) => {
                a.mv = v;
                self.strafe_dir = sd;
            }
            None => {
                a.mv = [0.0, 0.0];
            }
        }
    }

    /// Choose where to fight `p` from: in range, in sight of it, out of sight of everyone else,
    /// with a step into cover next to it.
    fn plan_fight(&mut self, p: &Th, want: f32, soon: bool) -> FightPlan {
        let phase = self.rand() * TAU;
        let me_p = [self.me.pos[0], self.me.pos[1]];
        let t = self.t;
        let zm = self.zm();
        let m = &self.map;
        let z = &self.zone;
        let muzzle_h = self.me.muzzle_h;
        let t_muzzle = [p.pos[0], p.pos[1], p.pos[2] + 7.0 * (1.0 + 0.05 * p.cubes as f32)];
        let t_chest = [p.pos[0], p.pos[1], p.pos[2] + 4.5];
        let others: Vec<[f32; 3]> = self.mechs.iter().filter(|(id, tr)| **id != p.id && t - tr.seen_t < 6.0).map(|(_, tr)| [tr.pos[0], tr.pos[1], tr.pos[2] + 7.0]).collect();
        let hidden = |q: [f32; 2], from: [f32; 3]| -> bool {
            let g = m.ground(q[0], q[1]);
            !m.los(from, [q[0], q[1], g + 4.5]) && !m.los(from, [q[0], q[1], g + 8.6])
        };
        let mut cands: Vec<([f32; 2], f32)> = Vec::with_capacity(64);
        cands.push((me_p, 3.0));
        if let Some(pl) = &self.plan {
            if pl.target == p.id && pl.has_los {
                cands.push((pl.peek, 6.0));
            }
        }
        for (ri, r) in [7.0f32, 14.0, 24.0, 36.0].iter().enumerate() {
            let n = 8 + 2 * ri;
            for k in 0..n {
                let a = phase + k as f32 * TAU / n as f32;
                cands.push(([me_p[0] + r * a.cos(), me_p[1] + r * a.sin()], 0.0));
            }
        }
        let mut scored: Vec<(f32, [f32; 2])> = Vec::with_capacity(cands.len());
        for (q, bonus) in cands {
            let c = m.cell_of(q[0], q[1]);
            if !m.passable(c) {
                continue;
            }
            let q = if bonus > 0.0 { q } else { m.center(c) };
            let g = m.surf[c];
            if (g - self.me.pos[2]).abs() > 6.0 {
                continue;
            }
            if !m.los([q[0], q[1], g + muzzle_h], t_chest) {
                continue;
            }
            let d = d2(&q, &p.pos);
            let trav = d2(&q, &me_p);
            let mut s = bonus - (d - want).abs() * 0.12 - trav * 0.25;
            if trav > 3.0 && !m.walk_clear(me_p[0], me_p[1], q[0], q[1]) {
                s -= 12.0;
            }
            for o in &others {
                if d2(&q, o) < 260.0 && m.los_coarse(*o, [q[0], q[1], g + 4.5]) {
                    s -= 22.0;
                }
            }
            if d2(&q, &z.c) > z.r - 6.0 {
                s -= 200.0;
            }
            if soon && z.nr - d2(&q, &z.nc) < zm {
                s -= 40.0;
            }
            for n in self.npcs.values() {
                if d2(&q, &n.pos) < 48.0 {
                    s -= 12.0;
                }
            }
            scored.push((s, q));
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut best: Option<(f32, [f32; 2], Option<[f32; 2]>)> = None;
        for (s0, q) in scored.iter().take(8) {
            // a covered step next to it
            let mut hide: Option<[f32; 2]> = None;
            let mut s = *s0;
            'h: for r in [4.0f32, 7.0, 10.0] {
                for k in 0..8 {
                    let a = k as f32 * TAU / 8.0;
                    let h = [q[0] + r * a.cos(), q[1] + r * a.sin()];
                    let c = m.cell_of(h[0], h[1]);
                    if !m.passable(c) || !m.walk_clear(q[0], q[1], h[0], h[1]) {
                        continue;
                    }
                    if d2(&h, &z.c) > z.r - 6.0 {
                        continue;
                    }
                    if hidden(h, t_muzzle) {
                        hide = Some(h);
                        s += 18.0 - r;
                        if others.iter().all(|o| hidden(h, *o)) {
                            s += 5.0;
                        }
                        break 'h;
                    }
                }
            }
            if best.map_or(true, |b| s > b.0) {
                best = Some((s, *q, hide));
            }
        }
        match best {
            Some((_, peek, hide)) => FightPlan { peek, hide, has_los: true, tick: self.dcount, target: p.id, tpos: [p.pos[0], p.pos[1]] },
            None => FightPlan { peek: me_p, hide: None, has_los: false, tick: self.dcount, target: p.id, tpos: [p.pos[0], p.pos[1]] },
        }
    }

    /// One decision of fight movement: out at the peek spot while a weapon is ready, in cover
    /// while the gun reloads and the laser recharges.
    fn fight_step(&mut self, a: &mut Act, p: &Th, want: f32, soon: bool, dodge: bool) {
        let me = self.me.clone();
        let me_p = [me.pos[0], me.pos[1]];
        let need = match &self.plan {
            None => true,
            Some(pl) => pl.target != p.id || self.dcount - pl.tick >= 5 || d2(&pl.tpos, &p.pos) > 12.0,
        };
        if need {
            let pl = self.plan_fight(p, want, soon);
            self.plan = Some(pl);
        }
        let pl = match self.plan {
            Some(pl) => pl,
            None => return,
        };
        if !pl.has_los {
            // nowhere near sees it. Strafe if it is in view anyway; go after it only to finish a
            // wounded target while we are healthy; otherwise wait for it to come to us.
            if p.los {
                self.fight_move(a, p.pos, want, false);
            } else if p.hp < 800.0 && me.hp > 0.5 * me.max_hp {
                self.go_to(a, [p.pos[0], p.pos[1]], 60.0, false);
            }
            return;
        }
        // a laser is charging at us: step behind cover until it has been let go
        if dodge {
            if let Some(h) = pl.hide {
                if d2(&me_p, &h) < 9.0 {
                    self.go_to(a, h, 0.8, false);
                    return;
                }
            }
        }
        let mut ready = false;
        for w in me.w.iter().flatten() {
            match w.kind {
                WK::Gun => ready |= w.reload <= 0.7,
                WK::Laser => ready |= w.cooldown <= 0.0 && w.charge >= w.charge_t - 0.5,
                WK::Lightning => ready |= p.d < w.range && me.energy > 15.0,
                _ => {}
            }
        }
        match pl.hide {
            Some(h) if !ready => self.go_to(a, h, 0.8, false),
            _ => {
                if d2(&me_p, &pl.peek) > 6.0 || !p.los {
                    self.go_to(a, pl.peek, 0.8, false);
                } else {
                    self.fight_move(a, p.pos, want, true);
                }
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
        // linear lead including vertical speed: best of the rules tried on public replay tracks
        let lat = 0.05f32;
        let at = |tt: f32| -> [f32; 3] { [tc[0] + tg.vel[0] * tt, tc[1] + tg.vel[1] * tt, tc[2] + tg.vel[2] * tt] };
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
                let rank = UPGRADE_ORDER.iter().position(|u| *u == id.as_str()).unwrap_or(100);
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
        // leave early: detours around buildings can halve the speed made good toward the circle
        let need = d_safe / (me.walk * 0.45) + 10.0;
        // once the zone has us moving, keep going until we are properly inside the next circle
        // (otherwise the bot dithers on its edge between "safe" and "late")
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
            ths.push(Th { id: *id, pos: tr.pos, vel: tr.vel, d, los, facing, hostile, hp: tr.hp, dps: dps_of(tr.gun, tr.laser, tr.lightning, tr.mortar, d, tr.cubes), cubes: tr.cubes, charging: tr.charging && tr.laser });
        }
        let combat: Vec<Th> = ths.iter().filter(|h| h.los && (h.hostile || (h.facing && h.d < 250.0) || h.d < 55.0)).cloned().collect();
        // target: finish the weak, prefer the near, and do not hop between targets on small differences
        let cur_tgt = self.last_tgt;
        let prio = |h: &Th| -> f32 { h.hp + 3.0 * h.d - if h.hostile { 150.0 } else { 0.0 } - if h.id == cur_tgt { 200.0 } else { 0.0 } };
        let mut primary: Option<Th> = None;
        for h in &combat {
            if primary.map_or(true, |p| prio(h) < prio(&p)) {
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
                if t < self.fight_until && hpf > 0.3 {
                    fight = true;
                }
                if hpf < 0.3 && ttk_them > 2.0 && !endgame {
                    fight = false;
                }
            } else {
                // practice ladder: every hunter has to be killed; back off only to regenerate
                let losing = ttk_me < ttk_them * 0.8;
                if !endgame && ((hpf < 0.4 && losing && p.hp > 250.0) || (combat.len() >= 3 && hpf < 0.6)) {
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
                self.evade_until = self.evade_until.max(t + 2.0);
            }
        } else if !evade && !self.regen_mode {
            // a lone mech in view that is not looking at us: open on its back when the odds are good
            let vis: Vec<&Th> = ths.iter().filter(|h| h.los && h.d < 200.0).collect();
            if vis.len() == 1 {
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

        // ---- movement goal
        let mut label = "hold";
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
        if urgent {
            label = "zone";
            self.loot = None;
            let g = if z.nr < 10.0 { Some(z.nc) } else { want_spot(self, false) };
            let g = g.unwrap_or(z.nc);
            self.go_to(&mut a, g, 1.5, false);
            a.boost = me.energy > 8.0 && (!me.in_zone || (t_out - t) < need * 0.75);
        } else if fight {
            label = "fight";
            self.loot = None;
            let p = primary.unwrap();
            self.last_tgt = p.id;
            // Long range is where the weaker house bots stop hitting (public replays: medium bots land
            // nothing past 120 m, hard ones 13%) while a led gun and a laser still do. Close in only
            // to finish a nearly dead target with lightning.
            let want = if my_light && p.hp < 500.0 && p.d < 70.0 { 30.0 } else { 145.0 };
            if COVER_STEPS {
                let dodge = combat.iter().any(|h| h.charging && h.facing);
                self.fight_step(&mut a, &p, want, soon, dodge);
            } else {
                self.fight_move(&mut a, p.pos, want, false);
            }
            // being run down: use the boost to keep the gap
            if p.d < 80.0 && want > 100.0 && me.energy > 35.0 && (a.mv[0].abs() + a.mv[1].abs()) > 0.5 {
                a.boost = true;
            }
        } else if evade {
            label = "evade";
            self.loot = None;
            let g = want_spot(self, true).unwrap_or(z.nc);
            self.go_to(&mut a, g, 1.5, false);
            let exposed = combat.iter().any(|h| h.los) || t - self.last_mech_hit_t < 2.0;
            a.boost = exposed && me.energy > 12.0;
        } else {
            let t0 = std::time::Instant::now();
            loot = self.choose_loot(soon);
            self.prof[2] = self.prof[2].max(t0.elapsed().as_secs_f32() * 1000.0);
            self.loot = loot;
            if let Some(l) = loot {
                label = "loot";
                let d = d2(&l.pos, &me.pos);
                let los = self.map.los(eye, [l.pos[0], l.pos[1], l.pos[2] + 1.5]);
                match l.kind {
                    LK::Npc => {
                        if d < 42.0 && los {
                            self.fight_move(&mut a, l.pos, 30.0, false);
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

        // ---- aim and fire
        let mut tgt: Option<Tgt> = None;
        let mut allow_laser = false;
        if let Some(p) = &primary {
            if fight || p.hostile || p.facing {
                let k = 1.0 + 0.05 * p.cubes as f32;
                tgt = Some(Tgt { pos: p.pos, vel: p.vel, r: 2.5 * k, h: 9.0 * k });
                allow_laser = fight;
            }
        }
        if tgt.is_none() {
            // a drone that is shooting at us
            if t - self.npc_hit_t < 3.0 {
                if let Some(n) = self.npcs.get(&self.npc_hit_by) {
                    if t - n.seen_t < 0.35 && d2(&n.pos, &me.pos) < 90.0 {
                        tgt = Some(Tgt { pos: n.pos, vel: n.vel, r: 1.8, h: 3.5 });
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
                                tgt = Some(Tgt { pos: n.pos, vel: n.vel, r: 1.8, h: 3.5 });
                                allow_laser = true;
                            }
                        }
                    }
                    LK::Crate if d < 48.0 => {
                        if self.crates.get(&l.id).map_or(false, |c| t - c.seen_t < 0.35) {
                            tgt = Some(Tgt { pos: l.pos, vel: [0.0; 3], r: 1.4, h: 2.4 });
                            allow_laser = true;
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(tg) = tgt {
            // charge the laser behind cover when we are about to step out to a spot that sees the target
            let precharge = fight && self.plan.map_or(false, |pl| pl.has_los && d2(&pl.peek, &me_p) < 12.0);
            self.aim_fire(&mut a, &tg, allow_laser, precharge);
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

        if std::env::var_os("MECH_DEBUG").is_some() {
            self.dbg = format!("goal=({:.0},{:.0}) spot={:?} path={}{} d_safe={:.0} t_out={:.1} need={:.1} urgent={} mv=({:.2},{:.2}) boost={}", self.path_goal[0], self.path_goal[1], self.spot.map(|s| (s[0] as i32, s[1] as i32)), self.path.len(), if self.path_found { "F" } else { "p" }, d_safe, t_out - t, need, urgent, a.mv[0], a.mv[1], a.boost);
        }
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
