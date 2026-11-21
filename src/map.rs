//! Static world model: terrain, solids, a 2 m navigation grid, line of sight and A*.
//!
//! Built once from `v2/world.json` and `v2/terrain.i16` (protocol.md section 11).
//! Vertex (i, j) of the terrain grid sits at x = minx + i, y = miny + j (1 m step).
//! Navigation cell (ci, cj) covers vertices 2ci..2ci+1 and is centred at minx + 2ci + 0.5.

use serde_json::Value;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Mech radius plus a safety margin: nav cells closer than this to a solid are blocked.
pub const R_INFLATE: f32 = 3.0;

pub struct Map {
    pub nx: usize,
    pub ny: usize,
    pub minx: f32,
    pub miny: f32,
    /// Terrain height per vertex.
    pub terr: Vec<f32>,
    /// max(terrain, top of a solid standing on the ground) per vertex: blocks shots and sight.
    pub gtop: Vec<f32>,
    /// Elevated solids (bridge decks with air below): blocked between ebase and etop.
    pub ebase: Vec<f32>,
    pub etop: Vec<f32>,
    /// Vertex is inside something a mech cannot walk through.
    pub solid: Vec<bool>,
    /// Walking surface per vertex (terrain, or the top of a low deck).
    pub walkh: Vec<f32>,
    pub gx: usize,
    pub gy: usize,
    pub blocked: Vec<bool>,
    /// Headroom under an elevated bridge over this cell (infinite elsewhere).
    pub clear: Vec<f32>,
    /// Headroom the mech needs right now (its height plus a margin); set by the brain.
    pub need_h: f32,
    pub surf: Vec<f32>,
    /// 0 = blocked, 1..3 = cells to the nearest blocked cell, 4 = further.
    pub wall_near: Vec<u8>,
    pub comp: Vec<u32>,
    pub main_comp: u32,
    pub bx0: f32,
    pub bx1: f32,
    pub by0: f32,
    pub by1: f32,
}

fn pip(ring: &[(f32, f32)], x: f32, y: f32) -> bool {
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = ring[i];
        let (xj, yj) = ring[j];
        if (yi > y) != (yj > y) {
            let xin = (xj - xi) * (y - yi) / (yj - yi) + xi;
            if x < xin {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

fn ring_of(v: &Value) -> Vec<(f32, f32)> {
    let mut out = Vec::new();
    if let Some(a) = v.as_array() {
        let mut k = 0;
        while k + 1 < a.len() {
            out.push((a[k].as_f64().unwrap_or(0.0) as f32, a[k + 1].as_f64().unwrap_or(0.0) as f32));
            k += 2;
        }
    }
    out
}

pub const NEI: [(isize, isize, u32); 8] = [
    (1, 0, 200),
    (-1, 0, 200),
    (0, 1, 200),
    (0, -1, 200),
    (1, 1, 283),
    (1, -1, 283),
    (-1, 1, 283),
    (-1, -1, 283),
];

impl Map {
    /// A flat, empty map: the fallback when the map files cannot be read.
    pub fn flat() -> Map {
        let nx = 1070;
        let ny = 724;
        Map::build(vec![0.0; nx * ny], nx, ny, -513.0, -334.0, [-512.38, 555.08, -333.09, 388.64], &Value::Null, R_INFLATE)
    }

    pub fn load(dir: &str) -> Result<Map, String> {
        Map::load_with(dir, R_INFLATE)
    }

    pub fn load_with(dir: &str, r_inflate: f32) -> Result<Map, String> {
        let wtxt = std::fs::read_to_string(format!("{}/v2/world.json", dir)).map_err(|e| format!("world.json: {}", e))?;
        let w: Value = serde_json::from_str(&wtxt).map_err(|e| format!("world.json: {}", e))?;
        let t = &w["terrain"];
        let nx = t["nx"].as_u64().ok_or("terrain.nx")? as usize;
        let ny = t["ny"].as_u64().ok_or("terrain.ny")? as usize;
        let minx = t["minX"].as_f64().ok_or("terrain.minX")? as f32;
        let miny = t["minY"].as_f64().ok_or("terrain.minY")? as f32;
        let scale = t["scale"].as_f64().unwrap_or(0.01) as f32;
        let step = t["step"].as_f64().unwrap_or(1.0);
        if (step - 1.0).abs() > 1e-6 {
            return Err(format!("unsupported terrain step {}", step));
        }
        let file = t["file"].as_str().unwrap_or("terrain.i16");
        let bytes = std::fs::read(format!("{}/v2/{}", dir, file)).map_err(|e| format!("terrain: {}", e))?;
        if bytes.len() < nx * ny * 2 || nx < 8 || ny < 8 {
            return Err("terrain file too short".into());
        }
        let mut terr = vec![0.0f32; nx * ny];
        for k in 0..nx * ny {
            terr[k] = i16::from_le_bytes([bytes[2 * k], bytes[2 * k + 1]]) as f32 * scale;
        }
        let b = &w["bounds"];
        let bounds = [
            b["minX"].as_f64().unwrap_or(minx as f64) as f32,
            b["maxX"].as_f64().unwrap_or((minx + nx as f32) as f64) as f32,
            b["minY"].as_f64().unwrap_or(miny as f64) as f32,
            b["maxY"].as_f64().unwrap_or((miny + ny as f32) as f64) as f32,
        ];
        Ok(Map::build(terr, nx, ny, minx, miny, bounds, &w["prisms"], r_inflate))
    }

    fn build(terr: Vec<f32>, nx: usize, ny: usize, minx: f32, miny: f32, bounds: [f32; 4], prisms: &Value, r_inflate: f32) -> Map {
        let n = nx * ny;
        let mut gtop = terr.clone();
        let mut walkh = terr.clone();
        let mut ebase = vec![f32::INFINITY; n];
        let mut etop = vec![f32::NEG_INFINITY; n];
        let mut solid = vec![false; n];
        let vx = |x: f32| -> isize { (x - minx).round() as isize };
        let vy = |y: f32| -> isize { (y - miny).round() as isize };
        if let Some(ps) = prisms.as_array() {
            for p in ps {
                let kind = p["kind"].as_str().unwrap_or("building");
                let base = p["base"].as_f64().unwrap_or(0.0) as f32;
                let top = p["top"].as_f64().unwrap_or(0.0) as f32;
                let ring = ring_of(&p["ring"]);
                if ring.len() < 3 {
                    continue;
                }
                let holes: Vec<Vec<(f32, f32)>> = p["holes"].as_array().map(|hs| hs.iter().map(ring_of).collect()).unwrap_or_default();
                let (mut x0, mut x1, mut y0, mut y1) = (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY);
                for &(x, y) in &ring {
                    x0 = x0.min(x);
                    x1 = x1.max(x);
                    y0 = y0.min(y);
                    y1 = y1.max(y);
                }
                let i0 = (vx(x0) - 1).clamp(0, nx as isize - 1) as usize;
                let i1 = (vx(x1) + 1).clamp(0, nx as isize - 1) as usize;
                let j0 = (vy(y0) - 1).clamp(0, ny as isize - 1) as usize;
                let j1 = (vy(y1) + 1).clamp(0, ny as isize - 1) as usize;
                let is_bridge = kind == "bridge";
                // A bridge whose deck is level with the ground around it (river crossings) is walked
                // on; a bridge high above its ends is an obstacle that shots pass under.
                let mut deck = false;
                if is_bridge {
                    let mut maxg = f32::NEG_INFINITY;
                    for &(x, y) in &ring {
                        let i = vx(x).clamp(0, nx as isize - 1) as usize;
                        let j = vy(y).clamp(0, ny as isize - 1) as usize;
                        maxg = maxg.max(terr[j * nx + i]);
                    }
                    deck = top - maxg <= 1.5;
                }
                for j in j0..=j1 {
                    let y = miny + j as f32;
                    for i in i0..=i1 {
                        let x = minx + i as f32;
                        if !pip(&ring, x, y) {
                            continue;
                        }
                        if holes.iter().any(|h| pip(h, x, y)) {
                            continue;
                        }
                        let k = j * nx + i;
                        let g = terr[k];
                        if is_bridge {
                            if deck {
                                walkh[k] = walkh[k].max(top);
                                gtop[k] = gtop[k].max(top);
                            } else {
                                // walked under when the headroom allows (checked per nav cell)
                                ebase[k] = ebase[k].min(base);
                                etop[k] = etop[k].max(top);
                            }
                        } else if top - g > 1.0 {
                            solid[k] = true;
                            gtop[k] = gtop[k].max(top);
                        } else if top - g > 0.2 {
                            walkh[k] = walkh[k].max(top);
                            gtop[k] = gtop[k].max(top);
                        }
                    }
                }
            }
        }
        let gx = nx / 2;
        let gy = ny / 2;
        let gn = gx * gy;
        let mut blocked = vec![false; gn];
        let mut clear = vec![f32::INFINITY; gn];
        let mut surf = vec![0.0f32; gn];
        let r2 = r_inflate * r_inflate;
        for cj in 0..gy {
            for ci in 0..gx {
                let c = cj * gx + ci;
                let mut b = false;
                'o: for bb in -3isize..=4 {
                    for aa in -3isize..=4 {
                        let dx = aa as f32 - 0.5;
                        let dy = bb as f32 - 0.5;
                        if dx * dx + dy * dy > r2 {
                            continue;
                        }
                        let vi = 2 * ci as isize + aa;
                        let vj = 2 * cj as isize + bb;
                        if vi < 0 || vj < 0 || vi >= nx as isize || vj >= ny as isize {
                            continue;
                        }
                        let k = vj as usize * nx + vi as usize;
                        if solid[k] {
                            b = true;
                            break 'o;
                        }
                        if ebase[k].is_finite() {
                            clear[c] = clear[c].min(ebase[k] - terr[k]);
                        }
                    }
                }
                let wx = minx + 2.0 * ci as f32 + 0.5;
                let wy = miny + 2.0 * cj as f32 + 0.5;
                if wx < bounds[0] + r_inflate + 0.5 || wx > bounds[1] - r_inflate - 0.5 || wy < bounds[2] + r_inflate + 0.5 || wy > bounds[3] - r_inflate - 0.5 {
                    b = true;
                }
                // an overhang lower than a mech with no cubes is a wall for everyone
                if clear[c] < 9.6 {
                    b = true;
                }
                blocked[c] = b;
                let mut h = f32::NEG_INFINITY;
                for (aa, bb) in [(0usize, 0usize), (1, 0), (0, 1), (1, 1)] {
                    let vi = (2 * ci + aa).min(nx - 1);
                    let vj = (2 * cj + bb).min(ny - 1);
                    h = h.max(walkh[vj * nx + vi]);
                }
                surf[c] = h;
            }
        }
        // distance (in cells, capped) to the nearest blocked cell
        let mut wall_near = vec![4u8; gn];
        let mut frontier: Vec<usize> = Vec::new();
        for c in 0..gn {
            if blocked[c] {
                wall_near[c] = 0;
                frontier.push(c);
            }
        }
        for d in 1u8..=3 {
            let mut next = Vec::new();
            for &c in &frontier {
                let ci = (c % gx) as isize;
                let cj = (c / gx) as isize;
                for &(dx, dy, _) in NEI.iter() {
                    let ni = ci + dx;
                    let nj = cj + dy;
                    if ni < 0 || nj < 0 || ni >= gx as isize || nj >= gy as isize {
                        continue;
                    }
                    let nn = nj as usize * gx + ni as usize;
                    if wall_near[nn] > d {
                        wall_near[nn] = d;
                        next.push(nn);
                    }
                }
            }
            frontier = next;
        }
        let mut m = Map { nx, ny, minx, miny, terr, gtop, ebase, etop, solid, walkh, gx, gy, blocked, clear, need_h: 9.4, surf, wall_near, comp: vec![0; gn], main_comp: 0, bx0: bounds[0], bx1: bounds[1], by0: bounds[2], by1: bounds[3] };
        m.label_components();
        m
    }

    fn label_components(&mut self) {
        let gn = self.gx * self.gy;
        let mut comp = vec![0u32; gn];
        let mut next_id = 0u32;
        let mut best = (0usize, 0u32);
        let mut stack: Vec<usize> = Vec::new();
        for s in 0..gn {
            if self.blocked[s] || comp[s] != 0 {
                continue;
            }
            next_id += 1;
            let mut size = 0usize;
            comp[s] = next_id;
            stack.push(s);
            while let Some(c) = stack.pop() {
                size += 1;
                let ci = (c % self.gx) as isize;
                let cj = (c / self.gx) as isize;
                for &(dx, dy, _) in NEI.iter() {
                    let ni = ci + dx;
                    let nj = cj + dy;
                    if ni < 0 || nj < 0 || ni >= self.gx as isize || nj >= self.gy as isize {
                        continue;
                    }
                    let nn = nj as usize * self.gx + ni as usize;
                    if self.blocked[nn] || comp[nn] != 0 {
                        continue;
                    }
                    let diag = dx != 0 && dy != 0;
                    if diag && (self.blocked[cj as usize * self.gx + ni as usize] || self.blocked[nj as usize * self.gx + ci as usize]) {
                        continue;
                    }
                    if !self.can_step(c, nn, diag) || !self.can_step(nn, c, diag) {
                        continue;
                    }
                    comp[nn] = next_id;
                    stack.push(nn);
                }
            }
            if size > best.0 {
                best = (size, next_id);
            }
        }
        self.comp = comp;
        self.main_comp = best.1;
    }

    /// Free of walls and with enough headroom for the mech as it is now.
    #[inline]
    pub fn passable(&self, c: usize) -> bool {
        !self.blocked[c] && self.clear[c] >= self.need_h
    }

    /// Can a mech walk from cell a up or down to neighbour b (the slope / ledge rule, kept conservative)?
    #[inline]
    pub fn can_step(&self, a: usize, b: usize, diag: bool) -> bool {
        let dh = self.surf[b] - self.surf[a];
        dh <= if diag { 2.6 } else { 1.9 }
    }

    #[inline]
    pub fn cell_xy(&self, x: f32, y: f32) -> (usize, usize) {
        let ci = (((x - self.minx + 0.5) * 0.5).floor() as isize).clamp(0, self.gx as isize - 1) as usize;
        let cj = (((y - self.miny + 0.5) * 0.5).floor() as isize).clamp(0, self.gy as isize - 1) as usize;
        (ci, cj)
    }

    #[inline]
    pub fn cell_of(&self, x: f32, y: f32) -> usize {
        let (ci, cj) = self.cell_xy(x, y);
        cj * self.gx + ci
    }

    #[inline]
    pub fn center(&self, c: usize) -> [f32; 2] {
        [self.minx + 2.0 * (c % self.gx) as f32 + 0.5, self.miny + 2.0 * (c / self.gx) as f32 + 0.5]
    }

    #[inline]
    fn vert(&self, x: f32, y: f32) -> Option<usize> {
        let i = (x - self.minx).round();
        let j = (y - self.miny).round();
        if i < 0.0 || j < 0.0 || i >= self.nx as f32 || j >= self.ny as f32 {
            return None;
        }
        Some(j as usize * self.nx + i as usize)
    }

    /// Ground or deck height under a point.
    pub fn ground(&self, x: f32, y: f32) -> f32 {
        match self.vert(x, y) {
            Some(k) => self.walkh[k],
            None => 0.0,
        }
    }

    /// Is the point inside terrain, a building or a bridge deck?
    pub fn point_blocked(&self, x: f32, y: f32, z: f32) -> bool {
        match self.vert(x, y) {
            Some(k) => z < self.gtop[k] - 0.3 || (z > self.ebase[k] && z < self.etop[k]),
            None => true,
        }
    }

    pub fn is_solid(&self, x: f32, y: f32) -> bool {
        match self.vert(x, y) {
            Some(k) => self.solid[k],
            None => true,
        }
    }

    /// Straight line between two points clear of terrain, buildings and bridge decks.
    /// The first and last 2 m are skipped (the shooter and the target stand there).
    pub fn los(&self, a: [f32; 3], b: [f32; 3]) -> bool {
        let dx = b[0] - a[0];
        let dy = b[1] - a[1];
        let dz = b[2] - a[2];
        let d = (dx * dx + dy * dy).sqrt();
        let n = d.ceil().max(1.0) as usize;
        for s in 1..n {
            let f = s as f32 / n as f32;
            let da = f * d;
            if da < 2.0 || d - da < 2.0 {
                continue;
            }
            let z = a[2] + dz * f;
            if let Some(k) = self.vert(a[0] + dx * f, a[1] + dy * f) {
                if z < self.gtop[k] - 0.3 {
                    return false;
                }
                if z > self.ebase[k] && z < self.etop[k] {
                    return false;
                }
            }
        }
        true
    }

    /// Coarser, cheaper line of sight (2 m steps) for scoring many candidate positions.
    pub fn los_coarse(&self, a: [f32; 3], b: [f32; 3]) -> bool {
        let dx = b[0] - a[0];
        let dy = b[1] - a[1];
        let dz = b[2] - a[2];
        let d = (dx * dx + dy * dy).sqrt();
        let n = (d * 0.5).ceil().max(1.0) as usize;
        for s in 1..n {
            let f = s as f32 / n as f32;
            let da = f * d;
            if da < 3.0 || d - da < 3.0 {
                continue;
            }
            let z = a[2] + dz * f;
            if let Some(k) = self.vert(a[0] + dx * f, a[1] + dy * f) {
                if z < self.gtop[k] - 0.3 || (z > self.ebase[k] && z < self.etop[k]) {
                    return false;
                }
            }
        }
        true
    }

    /// Can a mech walk the straight segment (every sampled cell free, no step too high)?
    pub fn walk_clear(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> bool {
        let dx = x1 - x0;
        let dy = y1 - y0;
        let d = (dx * dx + dy * dy).sqrt();
        let n = d.ceil().max(1.0) as usize;
        let mut prev = self.cell_of(x0, y0);
        for s in 1..=n {
            let f = s as f32 / n as f32;
            let c = self.cell_of(x0 + dx * f, y0 + dy * f);
            if c == prev {
                continue;
            }
            if !self.passable(c) {
                return false;
            }
            let dh = self.surf[c] - self.surf[prev];
            if dh > 1.9 || dh < -2.5 {
                return false;
            }
            prev = c;
        }
        true
    }

    /// Nearest free cell to `c`, searched in growing square rings.
    pub fn nearest_free(&self, c: usize, max_ring: isize, need_main: bool) -> Option<usize> {
        let ok = |k: usize| -> bool { self.passable(k) && (!need_main || self.comp[k] == self.main_comp) };
        if ok(c) {
            return Some(c);
        }
        let ci = (c % self.gx) as isize;
        let cj = (c / self.gx) as isize;
        for r in 1..=max_ring {
            let mut best: Option<(isize, usize)> = None;
            for dj in -r..=r {
                for di in -r..=r {
                    if di.abs() != r && dj.abs() != r {
                        continue;
                    }
                    let ni = ci + di;
                    let nj = cj + dj;
                    if ni < 0 || nj < 0 || ni >= self.gx as isize || nj >= self.gy as isize {
                        continue;
                    }
                    let k = nj as usize * self.gx + ni as usize;
                    if ok(k) {
                        let d = di * di + dj * dj;
                        if best.map_or(true, |b| d < b.0) {
                            best = Some((d, k));
                        }
                    }
                }
            }
            if let Some(b) = best {
                return Some(b.1);
            }
        }
        None
    }

    /// How many of 12 rays (36 m long) from this point hit a solid: a cheap measure of cover.
    pub fn cover(&self, x: f32, y: f32) -> u32 {
        let mut hits = 0;
        for r in 0..12 {
            let a = r as f32 * std::f32::consts::TAU / 12.0;
            let (s, c) = a.sin_cos();
            let mut d = 4.0;
            while d <= 36.0 {
                if self.is_solid(x + c * d, y + s * d) {
                    hits += 1;
                    break;
                }
                d += 2.0;
            }
        }
        hits
    }
}

/// Reusable A* scratch space.
pub struct Planner {
    g: Vec<u32>,
    from: Vec<u32>,
    stamp: Vec<u32>,
    epoch: u32,
    heap: BinaryHeap<Reverse<(u32, u32)>>,
}

impl Planner {
    pub fn new(n: usize) -> Planner {
        Planner { g: vec![0; n], from: vec![0; n], stamp: vec![0; n], epoch: 0, heap: BinaryHeap::new() }
    }

    /// A* over the nav grid. Returns the cell path from `start` and whether it reaches `goal`;
    /// when the expansion cap is hit the path leads to the explored cell nearest the goal.
    pub fn astar<F: Fn(usize) -> u32>(&mut self, m: &Map, start: usize, goal: usize, max_exp: usize, extra: F) -> (Vec<u32>, bool) {
        let gx = m.gx as isize;
        let gy = m.gy as isize;
        let (gi, gj) = ((goal % m.gx) as isize, (goal / m.gx) as isize);
        let h = |c: usize| -> u32 {
            let dx = ((c % m.gx) as isize - gi).unsigned_abs() as u32;
            let dy = ((c / m.gx) as isize - gj).unsigned_abs() as u32;
            let (lo, hi) = if dx < dy { (dx, dy) } else { (dy, dx) };
            (hi * 200 + lo * 83) * 13 / 10
        };
        if self.epoch == u32::MAX {
            self.stamp.iter_mut().for_each(|s| *s = 0);
            self.epoch = 0;
        }
        self.epoch += 1;
        let ep = self.epoch;
        self.heap.clear();
        self.g[start] = 0;
        self.from[start] = start as u32;
        self.stamp[start] = ep;
        self.heap.push(Reverse((h(start), start as u32)));
        let mut best = start;
        let mut best_h = h(start);
        let mut found = false;
        let mut exp = 0usize;
        while let Some(Reverse((f, c32))) = self.heap.pop() {
            let c = c32 as usize;
            let gc = self.g[c];
            let hc = h(c);
            if f > gc + hc {
                continue;
            }
            if c == goal {
                best = c;
                found = true;
                break;
            }
            if hc < best_h {
                best_h = hc;
                best = c;
            }
            exp += 1;
            if exp > max_exp {
                break;
            }
            let ci = (c % m.gx) as isize;
            let cj = (c / m.gx) as isize;
            for &(dx, dy, cost) in NEI.iter() {
                let ni = ci + dx;
                let nj = cj + dy;
                if ni < 0 || nj < 0 || ni >= gx || nj >= gy {
                    continue;
                }
                let n = nj as usize * m.gx + ni as usize;
                if !m.passable(n) {
                    continue;
                }
                let diag = dx != 0 && dy != 0;
                if diag && (m.blocked[cj as usize * m.gx + ni as usize] || m.blocked[nj as usize * m.gx + ci as usize]) {
                    continue;
                }
                let dh = m.surf[n] - m.surf[c];
                if dh > if diag { 2.6 } else { 1.9 } {
                    continue;
                }
                let mut step = cost + extra(n);
                match m.wall_near[n] {
                    1 => step += 110,
                    2 => step += 40,
                    _ => {}
                }
                if dh < -2.5 {
                    step += 4000;
                }
                let ng = gc + step;
                if self.stamp[n] != ep || ng < self.g[n] {
                    self.stamp[n] = ep;
                    self.g[n] = ng;
                    self.from[n] = c as u32;
                    self.heap.push(Reverse((ng + h(n), n as u32)));
                }
            }
        }
        let mut path = vec![best as u32];
        let mut c = best;
        let mut guard = 0;
        while c != start && guard < 400_000 {
            c = self.from[c] as usize;
            path.push(c as u32);
            guard += 1;
        }
        path.reverse();
        (path, found)
    }
}
