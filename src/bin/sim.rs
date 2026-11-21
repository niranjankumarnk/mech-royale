//! A small local simulator, written from protocol.md, to catch gross behaviour bugs (walking out of
//! the zone, getting stuck, never firing). It is NOT the organisers' simulator: movement is 2D with
//! wall sliding (no jumps or jets), house bots are simple scripts, sound and supply drops are left
//! out. Do not tune fine details against it.
//!
//!   sim <map_dir> [matches] [seed] [mode: practice|ffa] [mine] [verbose]

use mechbot::brain::Brain;
use mechbot::map::{Map, Planner};
use serde_json::{json, Value};
use std::f32::consts::{PI, TAU};

const DT: f32 = 0.05;
const PHASES: [(f32, f32, f32, f32); 6] = [(60.0, 50.0, 380.0, 20.0), (45.0, 40.0, 240.0, 35.0), (35.0, 35.0, 140.0, 60.0), (30.0, 30.0, 70.0, 100.0), (25.0, 25.0, 25.0, 160.0), (15.0, 25.0, 0.0, 260.0)];

struct Rng(u64);
impl Rng {
    fn f(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        ((x >> 40) as f32) / ((1u64 << 24) as f32)
    }
    fn gauss(&mut self) -> f32 {
        let u = self.f().max(1e-6);
        let v = self.f();
        (-2.0 * u.ln()).sqrt() * (TAU * v).cos()
    }
}

fn wrap(a: f32) -> f32 {
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

#[derive(Clone)]
struct Mech {
    pos: [f32; 3],
    vel: [f32; 2],
    aim: [f32; 2],
    hp: f32,
    max_hp: f32,
    energy: f32,
    alive: bool,
    cubes: i32,
    gun_cd: f32,
    ammo: i32,
    reload: f32,
    slot1: Option<&'static str>,
    w1_cd: f32,
    charge: f32,
    last_dmg_t: f32,
    last_spend_t: f32,
    mv: [f32; 2],
    boost: bool,
    cmd_aim: [f32; 2],
    fire: [bool; 2],
    tier: usize,
    path: Vec<u32>,
    path_t: f32,
    strafe: f32,
    strafe_t: f32,
    noise: [f32; 2],
    noise_t: f32,
    hold: [f32; 2],
    kills: i32,
    death_t: f32,
    killed_by: String,
    dmg_zone: f32,
    dmg_taken: f32,
    dmg_dealt: f32,
    stuck: i32,
    max_stuck: i32,
    cur_stuck: i32,
    seen_t: Vec<f32>,
}

struct Proj {
    id: i64,
    owner: i64,
    pos: [f32; 3],
    vel: [f32; 3],
    dmg: f32,
    ttl: f32,
    kind: &'static str,
}
struct Npc {
    id: i64,
    pos: [f32; 3],
    hp: f32,
    cd: f32,
}
struct Thing {
    id: i64,
    pos: [f32; 3],
    hp: f32,
    kind: &'static str,
}

struct World {
    map: Map,
    pl: Planner,
    rng: Rng,
    t: f32,
    tick: i64,
    mechs: Vec<Mech>,
    projs: Vec<Proj>,
    npcs: Vec<Npc>,
    crates: Vec<Thing>,
    cubes: Vec<Thing>,
    picks: Vec<Thing>,
    next_id: i64,
    zc: [f32; 2],
    zr: f32,
    centers: Vec<[f32; 2]>,
    events: Vec<Vec<Value>>,
    ffa: bool,
    mine: usize,
}

const TIER_SIGMA: [f32; 4] = [5.0, 3.0, 1.8, 0.7];
const TIER_RANGE: [f32; 4] = [70.0, 95.0, 150.0, 220.0];

impl World {
    fn zone(&self, t: f32) -> ([f32; 2], f32, [f32; 2], f32, f32, f32, f32) {
        // (center, radius, next_center, next_radius, shrink_start, shrink_end, dps)
        let mut t0 = 0.0;
        let mut c = self.centers[0];
        let mut r = 560.0;
        for (i, ph) in PHASES.iter().enumerate() {
            let s = t0 + ph.0;
            let e = s + ph.1;
            let nc = self.centers[i + 1];
            if t < e || i == PHASES.len() - 1 {
                let f = ((t - s) / ph.1).clamp(0.0, 1.0);
                return ([c[0] + (nc[0] - c[0]) * f, c[1] + (nc[1] - c[1]) * f], r + (ph.2 - r) * f, nc, ph.2, s, e, ph.3);
            }
            t0 = e;
            c = nc;
            r = ph.2;
        }
        unreachable!()
    }

    fn free(&self, x: f32, y: f32, z: f32) -> bool {
        if x < self.map.bx0 + 2.6 || x > self.map.bx1 - 2.6 || y < self.map.by0 + 2.6 || y > self.map.by1 - 2.6 {
            return false;
        }
        if self.map.is_solid(x, y) {
            return false;
        }
        for k in 0..8 {
            let a = k as f32 * TAU / 8.0;
            if self.map.is_solid(x + 2.4 * a.cos(), y + 2.4 * a.sin()) {
                return false;
            }
        }
        self.map.ground(x, y) - z <= 1.2
    }

    fn random_free(&mut self, c: [f32; 2], r: f32) -> [f32; 2] {
        for _ in 0..2000 {
            let a = self.rng.f() * TAU;
            let rr = r * self.rng.f().sqrt();
            let p = [c[0] + rr * a.cos(), c[1] + rr * a.sin()];
            let k = self.map.cell_of(p[0], p[1]);
            if !self.map.blocked[k] && self.map.comp[k] == self.map.main_comp && self.map.wall_near[k] >= 2 {
                return self.map.center(k);
            }
        }
        c
    }

    fn damage(&mut self, target: usize, amount: f32, weapon: &'static str, by: i64, from: [f32; 2]) {
        if !self.mechs[target].alive {
            return;
        }
        let m = &self.mechs[target];
        let bearing = (from[1] - m.pos[1]).atan2(from[0] - m.pos[0]);
        let rear = weapon != "zone" && wrap(bearing - m.aim[0]).abs() > PI / 2.0;
        let mut amt = amount * if rear { 1.5 } else { 1.0 };
        if by >= 0 && by < 100 {
            amt *= 1.0 + 0.04 * self.mechs[by as usize].cubes.min(10) as f32;
        }
        let t = self.t;
        let m = &mut self.mechs[target];
        m.hp -= amt;
        m.last_dmg_t = t;
        m.dmg_taken += amt;
        if weapon == "zone" {
            m.dmg_zone += amt;
        }
        let mut ev = json!({"type": "damaged", "amount": amt, "weapon": weapon, "bearing": if weapon == "zone" { Value::Null } else { json!(bearing) }, "by": if by >= 0 { json!(by) } else { Value::Null }});
        if rear {
            ev["rear"] = json!(true);
        }
        self.events[target].push(ev);
        if by >= 0 && by < 100 {
            let b = by as usize;
            self.mechs[b].last_dmg_t = t;
            self.mechs[b].dmg_dealt += amt;
            self.events[b].push(json!({"type": "hit", "target": target, "amount": amt, "weapon": weapon}));
        }
        if self.mechs[target].hp <= 0.0 {
            let m = &mut self.mechs[target];
            m.alive = false;
            m.death_t = t;
            m.killed_by = format!("{}:{}", weapon, by);
            let pos = m.pos;
            let n = m.cubes + 2;
            let s1 = m.slot1;
            for k in 0..n {
                self.next_id += 1;
                self.cubes.push(Thing { id: 5000 + self.next_id, pos: [pos[0] + (k as f32) * 1.5 - 2.0, pos[1] + ((k * 7) % 5) as f32 - 2.0, pos[2]], hp: 0.0, kind: "cube" });
            }
            if let Some(k) = s1 {
                self.next_id += 1;
                self.picks.push(Thing { id: 8000 + self.next_id, pos, hp: 0.0, kind: k });
            }
            if by >= 0 && by < 100 {
                self.mechs[by as usize].kills += 1;
                self.events[by as usize].push(json!({"type": "kill", "target": target}));
            }
        }
    }

    fn sees(&self, a: usize, p: [f32; 3], h: f32) -> bool {
        let m = &self.mechs[a];
        let d = d2(&m.pos, &p);
        if d > 200.0 {
            return false;
        }
        let eye = [m.pos[0], m.pos[1], m.pos[2] + 8.0];
        if d > 45.0 {
            let b = (p[1] - m.pos[1]).atan2(p[0] - m.pos[0]);
            if wrap(b - m.aim[0]).abs() > PI / 3.0 {
                return false;
            }
        }
        self.map.los(eye, [p[0], p[1], p[2] + h])
    }

    fn observe(&self, i: usize) -> Value {
        let m = &self.mechs[i];
        let (c, r, nc, nr, s, e, dps) = self.zone(self.t);
        let mut w1 = Value::Null;
        if let Some(k) = m.slot1 {
            w1 = match k {
                "laser" => json!({"kind": "laser", "cooldown": m.w1_cd.max(0.0), "charge": m.charge, "stats": {"damage": 315, "charge": 0.75, "min_charge": 0.15, "cooldown": 1.65, "range": 280}}),
                "lightning" => json!({"kind": "lightning", "cooldown": m.w1_cd.max(0.0), "stats": {"damage": 60, "cooldown": 0.25, "range": 45, "cone_deg": 30, "energy": 3}}),
                _ => json!({"kind": k, "cooldown": 0, "stats": {"damage": 200, "cooldown": 2.6, "speed": 72}}),
            };
        }
        let scale = 1.0 + 0.05 * m.cubes as f32;
        let mut mechs = Vec::new();
        for (j, o) in self.mechs.iter().enumerate() {
            if j == i || !o.alive {
                continue;
            }
            if self.t - m.seen_t[j] <= 5.0 {
                let mut ws = vec!["gun"];
                if let Some(k) = o.slot1 {
                    ws.push(k);
                }
                mechs.push(json!({"id": j, "kind": "mech", "pos": o.pos, "vel": [o.vel[0], o.vel[1], 0.0], "aim": o.aim, "hp": o.hp, "max_hp": o.max_hp, "cubes": o.cubes, "weapons": ws, "charging": o.charge > 0.0}));
            }
        }
        for n in &self.npcs {
            if self.sees(i, n.pos, 1.75) {
                mechs.push(json!({"id": n.id, "kind": "npc", "pos": n.pos, "vel": [0.0, 0.0, 0.0], "aim": [0.0, 0.0], "hp": n.hp, "max_hp": 350}));
            }
        }
        let vis = |v: &Vec<Thing>, withkind: bool, withhp: bool| -> Vec<Value> {
            v.iter()
                .filter(|c| self.sees(i, c.pos, 1.2))
                .map(|c| {
                    let mut o = json!({"id": c.id, "pos": c.pos});
                    if withkind {
                        o["kind"] = json!(c.kind);
                    }
                    if withhp {
                        o["hp"] = json!(c.hp);
                    }
                    o
                })
                .collect()
        };
        let projs: Vec<Value> = self.projs.iter().filter(|p| p.owner != i as i64 && d2(&p.pos, &m.pos) < 200.0).map(|p| json!({"id": p.id, "kind": p.kind, "pos": p.pos, "vel": p.vel})).collect();
        json!({
            "type": "obs", "tick": self.tick, "t": self.t, "alive": self.mechs.iter().filter(|x| x.alive).count(),
            "self": {"id": i, "pos": m.pos, "vel": [m.vel[0], m.vel[1], 0.0], "aim": m.aim, "hp": m.hp, "max_hp": m.max_hp, "energy": m.energy, "max_energy": 100,
                "grounded": true, "cubes": m.cubes, "upgrades": [], "in_zone": d2(&m.pos, &c) <= r,
                "weapons": [{"kind": "gun", "cooldown": m.gun_cd.max(0.0), "ammo": m.ammo, "reload": m.reload.max(0.0), "stats": {"damage": 45, "cooldown": 0.24, "speed": 252, "range": 230, "spread_deg": 0.6, "mag": 12, "reload": 2.4}}, w1],
                "stats": {"radius": 2.5 * scale, "height": 9.0 * scale, "eye_height": 8.0 * scale, "muzzle_height": 7.0 * scale, "walk_speed": 11.7, "boost_speed": 19.5, "turn_rate": 4, "pitch_rate": 3, "min_pitch": -0.7, "max_pitch": 1.3}},
            "mechs": mechs, "projectiles": projs, "crates": vis(&self.crates, false, true), "cubes": vis(&self.cubes, false, false), "pickups": vis(&self.picks, true, false),
            "sounds": [], "beams": [], "events": self.events[i].clone(), "drops": [],
            "zone": {"center": c, "radius": r, "next_center": nc, "next_radius": nr, "shrink_start": s, "shrink_end": e, "dps": dps},
            "offer": Value::Null
        })
    }

    fn apply(&mut self, i: usize, a: &Value) {
        let m = &mut self.mechs[i];
        let mv = [a["move"][0].as_f64().unwrap_or(0.0) as f32, a["move"][1].as_f64().unwrap_or(0.0) as f32];
        let n = (mv[0] * mv[0] + mv[1] * mv[1]).sqrt();
        m.mv = if n > 1.0 { [mv[0] / n, mv[1] / n] } else { mv };
        m.boost = a["boost"].as_bool().unwrap_or(false);
        if a["aim"].is_array() {
            m.cmd_aim = [a["aim"][0].as_f64().unwrap_or(0.0) as f32, (a["aim"][1].as_f64().unwrap_or(0.0) as f32).clamp(-0.7, 1.3)];
        }
        m.fire = [a["fire"][0].as_bool().unwrap_or(false), a["fire"][1].as_bool().unwrap_or(false)];
        if a["reload"].as_bool() == Some(true) && m.reload <= 0.0 && m.ammo < 12 {
            m.reload = 2.4;
            m.ammo = 0;
        }
        if let Some(id) = a["take"].as_i64() {
            let pos = m.pos;
            if let Some(k) = self.picks.iter().position(|p| p.id == id && d2(&p.pos, &pos) < 4.0) {
                let kind = self.picks[k].kind;
                let m = &mut self.mechs[i];
                let dropping = a["drop"].as_i64().is_some();
                if (m.slot1.is_none() || dropping) && m.slot1 != Some(kind) {
                    let old = m.slot1;
                    m.slot1 = Some(kind);
                    m.w1_cd = 1.65;
                    m.charge = 0.0;
                    self.picks.remove(k);
                    self.events[i].push(json!({"type": "took", "id": id, "kind": kind, "slot": 1}));
                    if let Some(o) = old {
                        self.next_id += 1;
                        self.picks.push(Thing { id: 8000 + self.next_id, pos, hp: 0.0, kind: o });
                    }
                }
            }
        }
    }

    /// Scripted house bot.
    fn house(&mut self, i: usize) {
        let t = self.t;
        let (_, _, nc, nr, s, _, _) = self.zone(t);
        let me = self.mechs[i].clone();
        // target
        let mut target: Option<usize> = None;
        if self.ffa {
            let mut bd = f32::INFINITY;
            for j in 0..self.mechs.len() {
                if j != i && self.mechs[j].alive && t - me.seen_t[j] < 3.0 {
                    let d = d2(&self.mechs[j].pos, &me.pos);
                    if d < bd {
                        bd = d;
                        target = Some(j);
                    }
                }
            }
        } else {
            // practice squad: weakest hunters first, one more per kill, everyone late
            let kills: i32 = (0..self.mine).map(|k| self.mechs[k].kills).sum();
            let mut order: Vec<usize> = (self.mine..self.mechs.len()).filter(|&j| self.mechs[j].alive).collect();
            order.sort_by_key(|&j| (self.mechs[j].tier, j));
            let n_hunt = if t > 240.0 { order.len() } else { (1 + kills as usize).min(order.len()) };
            if order.iter().take(n_hunt).any(|&j| j == i) {
                let mut bd = f32::INFINITY;
                for j in 0..self.mine {
                    if self.mechs[j].alive {
                        let d = d2(&self.mechs[j].pos, &me.pos);
                        if d < bd {
                            bd = d;
                            target = Some(j);
                        }
                    }
                }
            }
        }
        let mut goal = me.hold;
        let depth = nr - d2(&me.pos, &nc);
        let zone_urgent = depth < 0.3 * nr && t > s - 25.0;
        if d2(&me.hold, &nc) > nr * 0.65 {
            let h = self.random_free(nc, nr * 0.6);
            self.mechs[i].hold = h;
            goal = h;
        }
        let mut mv = [0.0f32, 0.0];
        let mut fire = [false, false];
        let mut aim = me.cmd_aim;
        let mut strafing = false;
        if let Some(j) = target {
            let tg = self.mechs[j].clone();
            let d = d2(&tg.pos, &me.pos);
            let los = self.map.los([me.pos[0], me.pos[1], me.pos[2] + 7.0], [tg.pos[0], tg.pos[1], tg.pos[2] + 4.5]);
            let pref = TIER_RANGE[me.tier] * 0.8;
            if !zone_urgent {
                if los && d < pref {
                    strafing = true;
                } else {
                    goal = [tg.pos[0], tg.pos[1]];
                }
            }
            if t > self.mechs[i].noise_t {
                let sg = TIER_SIGMA[me.tier].to_radians();
                self.mechs[i].noise = [self.rng.gauss() * sg, self.rng.gauss() * sg * 0.5];
                self.mechs[i].noise_t = t + 0.5;
            }
            let nz = self.mechs[i].noise;
            let tt = d / 252.0;
            let px = tg.pos[0] + tg.vel[0] * tt;
            let py = tg.pos[1] + tg.vel[1] * tt;
            let yaw = (py - me.pos[1]).atan2(px - me.pos[0]);
            let pitch = (tg.pos[2] + 4.5 - me.pos[2] - 7.0).atan2(d.max(1.0));
            aim = [yaw + nz[0], pitch + nz[1]];
            if los && d < TIER_RANGE[me.tier] && wrap(yaw + nz[0] - me.aim[0]).abs() < 0.06 {
                fire[0] = true;
            }
            if los && me.slot1 == Some("laser") {
                if me.charge < 0.75 && me.w1_cd <= 0.0 {
                    fire[1] = true;
                } else if me.charge >= 0.75 {
                    let direct = (tg.pos[1] - me.pos[1]).atan2(tg.pos[0] - me.pos[0]);
                    aim = [direct + nz[0] * 0.5, pitch];
                    fire[1] = wrap(direct - me.aim[0]).abs() > 0.03;
                }
            }
            if los && me.slot1 == Some("lightning") && d < 42.0 {
                fire[1] = true;
            }
            if strafing {
                if t > me.strafe_t {
                    self.mechs[i].strafe = if self.rng.f() < 0.5 { 1.0 } else { -1.0 };
                    self.mechs[i].strafe_t = t + 0.6 + self.rng.f() * 1.2;
                }
                let sd = self.mechs[i].strafe;
                let u = [(tg.pos[0] - me.pos[0]) / d.max(0.1), (tg.pos[1] - me.pos[1]) / d.max(0.1)];
                let back = if me.hp < 0.4 * me.max_hp { -0.8 } else { 0.0 };
                mv = [-sd * u[1] + back * u[0], sd * u[0] + back * u[1]];
            }
        }
        if !strafing && d2(&goal, &me.pos) > 4.0 {
            if me.path.is_empty() || t > me.path_t {
                let sc = self.map.nearest_free(self.map.cell_of(me.pos[0], me.pos[1]), 5, false);
                let gc = self.map.nearest_free(self.map.cell_of(goal[0], goal[1]), 8, false);
                if let (Some(sc), Some(gc)) = (sc, gc) {
                    let (p, _) = self.pl.astar(&self.map, sc, gc, 30_000, |_| 0);
                    self.mechs[i].path = p;
                }
                self.mechs[i].path_t = t + 1.0;
            }
            let path = &self.mechs[i].path;
            let mut tp = goal;
            if path.len() >= 2 {
                let mut i0 = 0;
                let mut bd = f32::INFINITY;
                for (k, &c) in path.iter().enumerate() {
                    let d = d2(&self.map.center(c as usize), &me.pos);
                    if d < bd {
                        bd = d;
                        i0 = k;
                    }
                }
                let k = (i0 + 3).min(path.len() - 1);
                tp = self.map.center(path[k] as usize);
            }
            let d = d2(&tp, &me.pos).max(0.1);
            mv = [(tp[0] - me.pos[0]) / d, (tp[1] - me.pos[1]) / d];
            if target.is_none() {
                aim = [mv[1].atan2(mv[0]), 0.0];
            }
        } else if target.is_none() {
            aim = [wrap(me.aim[0] + 0.6), 0.0];
        }
        let m = &mut self.mechs[i];
        m.mv = mv;
        m.cmd_aim = aim;
        m.fire = fire;
        m.boost = false;
        // "looting": better bots find a second weapon after a while
        if m.slot1.is_none() && t > 15.0 + 10.0 * (3 - m.tier.min(3)) as f32 && m.tier >= 1 {
            let r = self.rng.f();
            m.slot1 = if r < 0.45 { Some("laser") } else if r < 0.65 { Some("lightning") } else { m.slot1 };
        }
    }

    fn step(&mut self) {
        let t = self.t;
        let (zc, zr, _, _, _, _, dps) = self.zone(t);
        self.zc = zc;
        self.zr = zr;
        let n = self.mechs.len();
        for i in 0..n {
            if !self.mechs[i].alive {
                continue;
            }
            // aim
            {
                let m = &mut self.mechs[i];
                let dy = wrap(m.cmd_aim[0] - m.aim[0]).clamp(-4.0 * DT, 4.0 * DT);
                m.aim[0] = wrap(m.aim[0] + dy);
                let dp = (m.cmd_aim[1] - m.aim[1]).clamp(-3.0 * DT, 3.0 * DT);
                m.aim[1] += dp;
                m.gun_cd -= DT;
                m.w1_cd -= DT;
                if m.reload > 0.0 {
                    m.reload -= DT;
                    if m.reload <= 0.0 {
                        m.ammo = 12;
                    }
                }
            }
            // move
            let (pos, mut vel, mv, boost, charging, energy) = {
                let m = &self.mechs[i];
                (m.pos, m.vel, m.mv, m.boost, m.charge > 0.0, m.energy)
            };
            let moving = mv[0].abs() + mv[1].abs() > 0.05;
            let boosting = boost && energy > 0.5 && moving;
            let mut sp = if boosting { 19.5 } else { 11.7 };
            if charging {
                sp *= 0.5;
            }
            let want = [mv[0] * sp, mv[1] * sp];
            let dv = [want[0] - vel[0], want[1] - vel[1]];
            let dn = (dv[0] * dv[0] + dv[1] * dv[1]).sqrt();
            let maxd = 58.5 * DT;
            if dn > maxd {
                vel = [vel[0] + dv[0] / dn * maxd, vel[1] + dv[1] / dn * maxd];
            } else {
                vel = want;
            }
            let nx = pos[0] + vel[0] * DT;
            let ny = pos[1] + vel[1] * DT;
            let mut np = pos;
            if self.free(nx, ny, pos[2]) {
                np = [nx, ny, 0.0];
            } else if self.free(nx, pos[1], pos[2]) {
                np = [nx, pos[1], 0.0];
                vel[1] = 0.0;
            } else if self.free(pos[0], ny, pos[2]) {
                np = [pos[0], ny, 0.0];
                vel[0] = 0.0;
            } else {
                vel = [0.0, 0.0];
            }
            np[2] = self.map.ground(np[0], np[1]);
            {
                let m = &mut self.mechs[i];
                let spd = (vel[0] * vel[0] + vel[1] * vel[1]).sqrt();
                if moving && spd < 1.0 {
                    m.stuck += 1;
                    m.cur_stuck += 1;
                    m.max_stuck = m.max_stuck.max(m.cur_stuck);
                } else {
                    m.cur_stuck = 0;
                }
                m.pos = np;
                m.vel = vel;
                if boosting {
                    m.energy = (m.energy - 17.0 * DT).max(0.0);
                    m.last_spend_t = t;
                } else if t - m.last_spend_t > 0.6 {
                    m.energy = (m.energy + 18.0 * DT).min(100.0);
                }
            }
            // cubes
            let mut k = 0;
            while k < self.cubes.len() {
                if d2(&self.cubes[k].pos, &np) < 3.5 {
                    self.cubes.remove(k);
                    let m = &mut self.mechs[i];
                    m.cubes += 1;
                    if m.cubes <= 10 {
                        let add = 1400.0 * 0.04;
                        m.max_hp += add;
                        m.hp += add;
                    }
                    let c = m.cubes;
                    self.events[i].push(json!({"type": "pickup", "cubes": c}));
                } else {
                    k += 1;
                }
            }
            // weapons
            let m = self.mechs[i].clone();
            let dir = [m.aim[1].cos() * m.aim[0].cos(), m.aim[1].cos() * m.aim[0].sin(), m.aim[1].sin()];
            let muzzle = [m.pos[0] + dir[0] * 3.0, m.pos[1] + dir[1] * 3.0, m.pos[2] + 7.0 + dir[2] * 3.0];
            if m.fire[0] && m.gun_cd <= 0.0 && m.reload <= 0.0 && m.ammo > 0 {
                let sg = 0.6f32.to_radians();
                let yaw = m.aim[0] + self.rng.gauss() * sg;
                let pitch = m.aim[1] + self.rng.gauss() * sg;
                let d = [pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin(), pitch.sin()];
                self.next_id += 1;
                self.projs.push(Proj { id: self.next_id, owner: i as i64, pos: muzzle, vel: [d[0] * 252.0, d[1] * 252.0, d[2] * 252.0], dmg: 45.0, ttl: 230.0 / 252.0, kind: "gun" });
                let mm = &mut self.mechs[i];
                mm.gun_cd = 0.24;
                mm.ammo -= 1;
                if mm.ammo == 0 {
                    mm.reload = 2.4;
                }
            }
            match m.slot1 {
                Some("laser") => {
                    if m.fire[1] && m.w1_cd <= 0.0 && m.charge < 0.75 + 2.0 {
                        self.mechs[i].charge += DT;
                    } else if m.charge > 0.0 {
                        let frac = 0.35 + 0.65 * (m.charge / 0.75).min(1.0);
                        self.mechs[i].charge = 0.0;
                        self.mechs[i].w1_cd = 1.65;
                        // hitscan: first mech / drone / crate along the ray
                        let mut best: Option<(f32, i64)> = None;
                        let test = |pos: [f32; 3], r: f32, h: f32, id: i64, best: &mut Option<(f32, i64)>| {
                            let rel = [pos[0] - muzzle[0], pos[1] - muzzle[1]];
                            let along = rel[0] * m.aim[0].cos() + rel[1] * m.aim[0].sin();
                            if along < 0.0 || along > 280.0 {
                                return;
                            }
                            let perp = (rel[0] * -m.aim[0].sin() + rel[1] * m.aim[0].cos()).abs();
                            let z = muzzle[2] + along * m.aim[1].tan();
                            if perp < r && z > pos[2] && z < pos[2] + h && best.map_or(true, |b| along < b.0) {
                                *best = Some((along, id));
                            }
                        };
                        for (j, o) in self.mechs.iter().enumerate() {
                            if j != i && o.alive {
                                let sc = 1.0 + 0.05 * o.cubes as f32;
                                test(o.pos, 2.5 * sc, 9.0 * sc, j as i64, &mut best);
                            }
                        }
                        for nn in &self.npcs {
                            test(nn.pos, 1.8, 3.5, nn.id, &mut best);
                        }
                        for c in &self.crates {
                            test(c.pos, 1.4, 2.4, c.id, &mut best);
                        }
                        if let Some((along, id)) = best {
                            let hitp = [muzzle[0] + dir[0] * along, muzzle[1] + dir[1] * along, muzzle[2] + dir[2] * along];
                            if self.map.los(muzzle, hitp) {
                                self.hit_id(id, 315.0 * frac, "laser", i as i64, [m.pos[0], m.pos[1]]);
                            }
                        }
                    }
                }
                Some("lightning") => {
                    if m.fire[1] && m.w1_cd <= 0.0 && m.energy >= 3.0 {
                        self.mechs[i].w1_cd = 0.25;
                        self.mechs[i].energy -= 3.0;
                        self.mechs[i].last_spend_t = t;
                        let mut best: Option<(f32, i64)> = None;
                        let mut cand: Vec<([f32; 3], i64)> = Vec::new();
                        for (j, o) in self.mechs.iter().enumerate() {
                            if j != i && o.alive {
                                cand.push(([o.pos[0], o.pos[1], o.pos[2] + 4.5], j as i64));
                            }
                        }
                        for nn in &self.npcs {
                            cand.push(([nn.pos[0], nn.pos[1], nn.pos[2] + 1.7], nn.id));
                        }
                        for c in &self.crates {
                            cand.push(([c.pos[0], c.pos[1], c.pos[2] + 1.2], c.id));
                        }
                        for (p, id) in cand {
                            let d = d2(&p, &m.pos);
                            let b = (p[1] - m.pos[1]).atan2(p[0] - m.pos[0]);
                            if d < 45.0 && wrap(b - m.aim[0]).abs() < 15f32.to_radians() && self.map.los([m.pos[0], m.pos[1], m.pos[2] + 7.0], p) && best.map_or(true, |x| d < x.0) {
                                best = Some((d, id));
                            }
                        }
                        if let Some((_, id)) = best {
                            self.hit_id(id, 60.0, "lightning", i as i64, [m.pos[0], m.pos[1]]);
                        }
                    }
                }
                _ => {}
            }
            // zone and regen
            if d2(&np, &zc) > zr {
                self.damage(i, dps * DT, "zone", -1, [np[0], np[1]]);
            } else {
                let m = &mut self.mechs[i];
                if m.alive && t - m.last_dmg_t > 5.0 {
                    m.hp = (m.hp + 45.0 * DT).min(m.max_hp);
                }
            }
        }
        // drones: shoot the nearest mech in range
        for k in 0..self.npcs.len() {
            self.npcs[k].cd -= DT;
            if self.npcs[k].cd > 0.0 {
                continue;
            }
            let np = self.npcs[k].pos;
            let mut best: Option<(f32, usize)> = None;
            for (j, o) in self.mechs.iter().enumerate() {
                let d = d2(&o.pos, &np);
                if o.alive && d < 45.0 && best.map_or(true, |b| d < b.0) && self.map.los([np[0], np[1], np[2] + 2.0], [o.pos[0], o.pos[1], o.pos[2] + 4.5]) {
                    best = Some((d, j));
                }
            }
            if let Some((d, j)) = best {
                let o = &self.mechs[j];
                let yaw = (o.pos[1] - np[1]).atan2(o.pos[0] - np[0]) + self.rng.gauss() * 2.5f32.to_radians();
                let pitch = (o.pos[2] + 4.5 - np[2] - 2.0).atan2(d.max(1.0));
                self.next_id += 1;
                let id = self.npcs[k].id;
                self.projs.push(Proj { id: self.next_id, owner: id, pos: [np[0], np[1], np[2] + 2.0], vel: [pitch.cos() * yaw.cos() * 90.0, pitch.cos() * yaw.sin() * 90.0, pitch.sin() * 90.0], dmg: 25.0, ttl: 2.0, kind: "npc" });
                self.npcs[k].cd = 1.0;
            }
        }
        // projectiles
        let mut k = 0;
        while k < self.projs.len() {
            let (prev, vel, owner, dmg, kind) = {
                let p = &self.projs[k];
                (p.pos, p.vel, p.owner, p.dmg, p.kind)
            };
            let new = [prev[0] + vel[0] * DT, prev[1] + vel[1] * DT, prev[2] + vel[2] * DT];
            let mut dead = false;
            let mut hit: Option<(f32, i64)> = None;
            let seg = [new[0] - prev[0], new[1] - prev[1]];
            let sl2 = (seg[0] * seg[0] + seg[1] * seg[1]).max(1e-6);
            let test = |pos: [f32; 3], r: f32, h: f32, id: i64, hit: &mut Option<(f32, i64)>| {
                let f = (((pos[0] - prev[0]) * seg[0] + (pos[1] - prev[1]) * seg[1]) / sl2).clamp(0.0, 1.0);
                let c = [prev[0] + seg[0] * f, prev[1] + seg[1] * f];
                let z = prev[2] + (new[2] - prev[2]) * f;
                if d2(&c, &pos) < r + 0.3 && z > pos[2] - 0.3 && z < pos[2] + h + 0.3 && hit.map_or(true, |b| f < b.0) {
                    *hit = Some((f, id));
                }
            };
            for (j, o) in self.mechs.iter().enumerate() {
                if j as i64 != owner && o.alive {
                    let sc = 1.0 + 0.05 * o.cubes as f32;
                    test(o.pos, 2.5 * sc, 9.0 * sc, j as i64, &mut hit);
                }
            }
            if owner < 100 {
                for nn in &self.npcs {
                    test(nn.pos, 1.8, 3.5, nn.id, &mut hit);
                }
                for c in &self.crates {
                    test(c.pos, 1.4, 2.4, c.id, &mut hit);
                }
            }
            for f in [0.33f32, 0.66, 1.0] {
                if self.map.point_blocked(prev[0] + seg[0] * f, prev[1] + seg[1] * f, prev[2] + (new[2] - prev[2]) * f) {
                    if hit.map_or(true, |h| h.0 > f) {
                        hit = None;
                        dead = true;
                    }
                    break;
                }
            }
            if let Some((_, id)) = hit {
                self.hit_id(id, dmg, kind, owner, [prev[0] - vel[0], prev[1] - vel[1]]);
                dead = true;
            }
            let p = &mut self.projs[k];
            p.pos = new;
            p.ttl -= DT;
            if dead || p.ttl <= 0.0 {
                self.projs.swap_remove(k);
            } else {
                k += 1;
            }
        }
        self.t += DT;
        self.tick += 1;
    }

    fn hit_id(&mut self, id: i64, dmg: f32, weapon: &'static str, by: i64, from: [f32; 2]) {
        if id < 100 {
            self.damage(id as usize, dmg, weapon, by, from);
        } else if id < 1000 {
            if let Some(k) = self.npcs.iter().position(|n| n.id == id) {
                self.npcs[k].hp -= dmg;
                if by >= 0 && by < 100 {
                    self.events[by as usize].push(json!({"type": "hit", "target": id, "amount": dmg, "weapon": weapon}));
                    self.mechs[by as usize].last_dmg_t = self.t;
                }
                if self.npcs[k].hp <= 0.0 {
                    let pos = self.npcs[k].pos;
                    self.npcs.remove(k);
                    for c in 0..2 {
                        self.next_id += 1;
                        self.cubes.push(Thing { id: 5000 + self.next_id, pos: [pos[0] + c as f32 * 2.0, pos[1], pos[2]], hp: 0.0, kind: "cube" });
                    }
                }
            }
        } else if let Some(k) = self.crates.iter().position(|c| c.id == id) {
            self.crates[k].hp -= dmg;
            if by >= 0 && by < 100 {
                self.events[by as usize].push(json!({"type": "hit", "target": id, "amount": dmg, "weapon": weapon}));
            }
            if self.crates[k].hp <= 0.0 {
                let pos = self.crates[k].pos;
                self.crates.remove(k);
                self.next_id += 1;
                self.cubes.push(Thing { id: 5000 + self.next_id, pos, hp: 0.0, kind: "cube" });
                if self.rng.f() < 0.5 {
                    self.next_id += 1;
                    let r = self.rng.f();
                    self.picks.push(Thing { id: 8000 + self.next_id, pos: [pos[0] + 2.0, pos[1], pos[2]], hp: 0.0, kind: if r < 0.33 { "mortar" } else if r < 0.66 { "laser" } else { "lightning" } });
                }
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = args.get(1).cloned().unwrap_or_else(|| "/arena".into());
    let matches: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(4);
    let seed0: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
    let ffa = args.get(4).map_or(false, |s| s == "ffa");
    let mine: usize = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(1);
    let verbose = args.get(6).is_some();
    let mut total = 0.0;
    let mut worst_ms = 0.0f64;
    let mut times: Vec<f32> = Vec::new();
    for mi in 0..matches {
        let seed = seed0 + mi;
        let map = Map::load(&dir).expect("map");
        let n = map.gx * map.gy;
        let mut w = World { map, pl: Planner::new(n), rng: Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1), t: 0.0, tick: 0, mechs: Vec::new(), projs: Vec::new(), npcs: Vec::new(), crates: Vec::new(), cubes: Vec::new(), picks: Vec::new(), next_id: 0, zc: [41.3, -72.8], zr: 560.0, centers: vec![[41.3, -72.8]], events: vec![Vec::new(); 16], ffa, mine };
        for _ in 0..8 {
            w.rng.f();
        }
        let mut r = 560.0;
        for ph in PHASES.iter() {
            let c = *w.centers.last().unwrap();
            let a = w.rng.f() * TAU;
            let rr = (r - ph.2) * w.rng.f().sqrt() * 0.9;
            w.centers.push([c[0] + rr * a.cos(), c[1] + rr * a.sin()]);
            r = ph.2;
        }
        // spawns
        let mut spawns: Vec<[f32; 2]> = Vec::new();
        let mut guard = 0;
        while spawns.len() < 16 && guard < 20000 {
            guard += 1;
            let p = w.random_free([41.3, -72.8], 560.0 * 0.8);
            if spawns.iter().all(|s| d2(s, &p) > 130.0) {
                spawns.push(p);
            }
        }
        while spawns.len() < 16 {
            let p = w.random_free([41.3, -72.8], 400.0);
            spawns.push(p);
        }
        // put our bots on random slots' spawns
        for i in 0..16usize {
            let p = spawns[i];
            let z = w.map.ground(p[0], p[1]);
            let tier = if i < mine { 3 } else { [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3][(i - mine) % 15] };
            let yaw = w.rng.f() * TAU - PI;
            w.mechs.push(Mech { pos: [p[0], p[1], z], vel: [0.0; 2], aim: [yaw, 0.0], hp: 1400.0, max_hp: 1400.0, energy: 100.0, alive: true, cubes: 0, gun_cd: 0.0, ammo: 12, reload: 0.0, slot1: None, w1_cd: 0.0, charge: 0.0, last_dmg_t: -10.0, last_spend_t: -10.0, mv: [0.0; 2], boost: false, cmd_aim: [yaw, 0.0], fire: [false; 2], tier, path: Vec::new(), path_t: 0.0, strafe: 1.0, strafe_t: 0.0, noise: [0.0; 2], noise_t: 0.0, hold: p, kills: 0, death_t: -1.0, killed_by: String::new(), dmg_zone: 0.0, dmg_taken: 0.0, dmg_dealt: 0.0, stuck: 0, max_stuck: 0, cur_stuck: 0, seen_t: vec![-100.0; 16] });
        }
        for k in 0..12 {
            let p = w.random_free([41.3, -72.8], 480.0);
            let z = w.map.ground(p[0], p[1]);
            w.npcs.push(Npc { id: 100 + k, pos: [p[0], p[1], z], hp: 350.0, cd: 0.0 });
        }
        for k in 0..27 {
            let p = w.random_free([41.3, -72.8], 480.0);
            let z = w.map.ground(p[0], p[1]);
            w.crates.push(Thing { id: 1000 + k, pos: [p[0], p[1], z], hp: 300.0, kind: "crate" });
        }
        for k in 0..30 {
            let p = w.random_free([41.3, -72.8], 480.0);
            let z = w.map.ground(p[0], p[1]);
            let kind = ["mortar", "laser", "lightning"][(w.rng.f() * 3.0) as usize % 3];
            w.picks.push(Thing { id: 8000 + k, pos: [p[0], p[1], z], hp: 0.0, kind });
        }
        w.next_id = 100;
        let hello = json!({"type": "hello", "slot": 0, "seed": seed, "map": {"dir": dir}, "config": {"loot": {"pickup_radius": 4}}});
        let mut brains: Vec<Brain> = (0..mine).map(|k| { let mut h = hello.clone(); h["seed"] = json!(seed * 31 + k as u64); Brain::new(&h) }).collect();
        let mut labels: std::collections::BTreeMap<&'static str, u32> = std::collections::BTreeMap::new();
        let mut place = vec![0usize; 16];
        let mut trace: Vec<String> = Vec::new();
        while w.t < 480.0 {
            if w.tick % 2 == 0 {
                // vision memory
                for i in 0..16 {
                    if !w.mechs[i].alive {
                        continue;
                    }
                    for j in 0..16 {
                        if i != j && w.mechs[j].alive && w.sees(i, w.mechs[j].pos, 4.5) {
                            w.mechs[i].seen_t[j] = w.t;
                        }
                    }
                }
                for i in 0..16 {
                    if !w.mechs[i].alive {
                        continue;
                    }
                    if i < mine {
                        let obs = w.observe(i);
                        w.events[i].clear();
                        let t0 = std::time::Instant::now();
                        let act = brains[i].decide(&obs);
                        let ms = t0.elapsed().as_secs_f64() * 1000.0;
                        if ms > worst_ms {
                            worst_ms = ms;
                        }
                        times.push(ms as f32);
                        assert_eq!(act["tick"], obs["tick"]);
                        if i == 0 {
                            *labels.entry(brains[0].label).or_insert(0) += 1;
                            if verbose && std::env::var_os("MECH_DEBUG").is_some() && w.tick % 10 == 0 {
                                let m = &w.mechs[0];
                                let (c, r, nc, nr, _, _, _) = w.zone(w.t);
                                trace.push(format!("t={:5.1} pos=({:6.1},{:6.1}) {} zin={:5.1} nin={:5.1} | {}", w.t, m.pos[0], m.pos[1], brains[0].label, r - d2(&m.pos, &c), nr - d2(&m.pos, &nc), brains[0].dbg));
                            }
                            if verbose && w.tick % 100 == 0 {
                                let m = &w.mechs[0];
                                let (c, r, _, _, _, _, _) = w.zone(w.t);
                                trace.push(format!("t={:5.1} pos=({:6.1},{:6.1}) hp={:4.0}/{:4.0} {} w1={:?} cubes={} alive={} zone_in={:5.1} ffa={}", w.t, m.pos[0], m.pos[1], m.hp, m.max_hp, brains[0].label, m.slot1, m.cubes, w.mechs.iter().filter(|x| x.alive).count(), r - d2(&m.pos, &c), brains[0].ffa));
                            }
                        }
                        w.apply(i, &act);
                    } else {
                        w.events[i].clear();
                        w.house(i);
                    }
                }
            }
            let before: Vec<bool> = w.mechs.iter().map(|m| m.alive).collect();
            w.step();
            let alive_now = w.mechs.iter().filter(|m| m.alive).count();
            for i in 0..16 {
                if before[i] && !w.mechs[i].alive {
                    place[i] = alive_now + 1;
                }
            }
            if alive_now <= 1 || !(0..mine).any(|i| w.mechs[i].alive) {
                break;
            }
        }
        for i in 0..16 {
            if w.mechs[i].alive {
                place[i] = 1;
            }
        }
        for i in 0..mine {
            let m = &w.mechs[i];
            let pts = 16 - place[i];
            total += pts as f32;
            println!("match {} seed {} bot {}: place {:2} pts {:2} kills {} death_t {:6.1} by {:14} dealt {:6.0} taken {:6.0} zone_dmg {:5.0} stuck_ticks {} max_stuck {} w1 {:?} cubes {} labels {:?}", mi, seed, i, place[i], pts, m.kills, m.death_t, m.killed_by, m.dmg_dealt, m.dmg_taken, m.dmg_zone, m.stuck, m.max_stuck, m.slot1, m.cubes, if i == 0 { format!("{:?}", labels) } else { String::new() });
        }
        println!("   prof ms (ingest, spot, loot, astar, total): {:?}", brains[0].prof);
        if verbose {
            for l in &trace {
                println!("   {}", l);
            }
        }
    }
    if let Ok(path) = std::env::var("SIM_TIMES") {
        let txt: Vec<String> = times.iter().map(|x| format!("{:.4}", x)).collect();
        let _ = std::fs::write(path, txt.join("\n"));
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f32| times[((times.len() as f32 - 1.0) * p) as usize];
    println!("mean points {:.2} over {} bot-matches; decision ms p50 {:.3} p99 {:.3} p99.9 {:.3} max {:.2}; over 5 ms: {} of {}", total / (matches as f32 * mine as f32), matches as usize * mine, pct(0.5), pct(0.99), pct(0.999), worst_ms, times.iter().filter(|x| **x > 5.0).count(), times.len());
}
