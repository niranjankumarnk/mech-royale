//! Dev tool: compare the map model with positions real mechs occupied in public replays.
//! Reads "x y z grounded" lines on stdin.  mapcheck <map_dir>
use mechbot::map::Map;
use std::io::BufRead;

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "/arena".into());
    let r: f32 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(mechbot::map::R_INFLATE);
    let m = Map::load_with(&dir, r).expect("map");
    // optional ASCII window: mapcheck <dir> <r> <cx> <cy> <half>
    let av: Vec<f32> = std::env::args().skip(3).filter_map(|s| s.parse().ok()).collect();
    if av.len() >= 3 {
        let (cx, cy, half) = (av[0], av[1], av[2]);
        let mut y = cy + half;
        while y >= cy - half {
            let mut row = String::new();
            let mut hrow = String::new();
            let mut x = cx - half;
            while x <= cx + half {
                let c = m.cell_of(x, y);
                let ch = if m.blocked[c] { if m.is_solid(x, y) { '#' } else { '+' } } else if m.comp[c] == m.main_comp { '.' } else { (b'a' + (m.comp[c] % 26) as u8) as char };
                row.push(ch);
                hrow.push_str(&format!("{:4.0}", m.surf[c]));
                x += 2.0;
            }
            println!("{:7.1} {}   {}", y, row, if half <= 24.0 { hrow } else { String::new() });
            y -= 2.0;
        }
        return;
    }
    if std::env::args().nth(3).as_deref() == Some("roofs") {
        println!("{} usable roofs", m.roofs.len());
        let mut rs: Vec<&mechbot::map::Roof> = m.roofs.iter().collect();
        rs.sort_by(|a, b| b.area.cmp(&a.area));
        for r in rs.iter().take(60) {
            println!("  top {:6.1} climb {:5.1} m  area {:6} m2  centre ({:7.1},{:7.1}) depth {:4.1} m", r.top, r.top - r.ground, r.area, r.center[0], r.center[1], r.depth);
        }
        // compare with replay samples: stdin lines "x y z grounded"
        let (mut n, mut close, mut on_listed) = (0u32, 0u32, 0u32);
        let mut diffs: Vec<f32> = Vec::new();
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            let v: Vec<f32> = line.split_whitespace().filter_map(|s| s.parse().ok()).collect();
            if v.len() < 4 || v[3] < 0.5 || !m.is_solid(v[0], v[1]) || v[2] < m.ground(v[0], v[1]) + 3.0 {
                continue;
            }
            n += 1;
            let d = v[2] - m.support(v[0], v[1]);
            diffs.push(d);
            if d.abs() < 0.6 {
                close += 1;
            }
            if m.roof_at(v[0], v[1]).is_some() {
                on_listed += 1;
            }
        }
        diffs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if n > 0 {
            println!("replay samples standing on solids: {}  within 0.6 m of my roof height: {} ({:.0}%)  on a listed roof: {} ({:.0}%)  z - my roof: p10 {:.1} p50 {:.1} p90 {:.1}", n, close, 100.0 * close as f32 / n as f32, on_listed, 100.0 * on_listed as f32 / n as f32, diffs[diffs.len() / 10], diffs[diffs.len() / 2], diffs[diffs.len() * 9 / 10]);
        }
        return;
    }
    if std::env::args().nth(3).as_deref() == Some("seen") {
        // For every mech of a recorded match: how long was it in another mech's view?
        // mapcheck <dir> <r> seen <match.json>
        let path = std::env::args().nth(4).expect("match json");
        let rec: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let frames = rec["frames"].as_array().unwrap();
        let mut seen: std::collections::BTreeMap<i64, [f32; 3]> = std::collections::BTreeMap::new();
        for fr in frames {
            let t = fr["t"].as_f64().unwrap() as f32 / 20.0;
            let rows: Vec<(i64, [f32; 3], f32)> = fr["m"].as_array().unwrap().iter().filter(|r| r[6].as_f64().unwrap_or(0.0) > 0.0).map(|r| (r[0].as_i64().unwrap(), [r[1].as_f64().unwrap() as f32, r[2].as_f64().unwrap() as f32, r[3].as_f64().unwrap() as f32], r[4].as_f64().unwrap() as f32)).collect();
            for a in &rows {
                let mut any = false;
                for b in &rows {
                    if a.0 == b.0 {
                        continue;
                    }
                    let d = ((a.1[0] - b.1[0]).powi(2) + (a.1[1] - b.1[1]).powi(2)).sqrt();
                    if d > 200.0 {
                        continue;
                    }
                    let bearing = (a.1[1] - b.1[1]).atan2(a.1[0] - b.1[0]);
                    let mut off = (bearing - b.2) % std::f32::consts::TAU;
                    if off > std::f32::consts::PI {
                        off -= std::f32::consts::TAU;
                    }
                    if off < -std::f32::consts::PI {
                        off += std::f32::consts::TAU;
                    }
                    if d > 45.0 && off.abs() > std::f32::consts::PI / 3.0 {
                        continue;
                    }
                    if m.los([b.1[0], b.1[1], b.1[2] + 8.0], [a.1[0], a.1[1], a.1[2] + 4.5]) {
                        any = true;
                        break;
                    }
                }
                if any {
                    let k = if t < 30.0 { 0 } else if t < 90.0 { 1 } else { 2 };
                    seen.entry(a.0).or_insert([0.0; 3])[k] += 0.1;
                }
            }
        }
        for mm in rec["mechs"].as_array().unwrap() {
            let id = mm["id"].as_i64().unwrap();
            let s = seen.get(&id).cloned().unwrap_or([0.0; 3]);
            let place = rec["places"][id.to_string()].as_i64().unwrap_or(0);
            let death = rec["deaths"].as_array().unwrap().iter().find(|d| d[1].as_i64() == Some(id)).map(|d| d[0].as_f64().unwrap() / 20.0).unwrap_or(-1.0);
            println!("REAL {} place {:2} died {:6.1} seen {:4.1} {:4.1} {:5.1} {}", rec["name"].as_str().unwrap_or(""), place, death, s[0], s[1], s[2], mm["name"].as_str().unwrap_or(""));
        }
        return;
    }
    if std::env::args().nth(3).as_deref() == Some("label") {
        // per input line: comp id (0 = blocked cell), is_main, solid, my ground height
        let mut out = String::new();
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            let v: Vec<f32> = line.split_whitespace().filter_map(|s| s.parse().ok()).collect();
            if v.len() < 2 {
                continue;
            }
            let c = m.cell_of(v[0], v[1]);
            out.push_str(&format!("{} {} {} {:.2}\n", if m.blocked[c] { 0 } else { m.comp[c] }, (m.comp[c] == m.main_comp && !m.blocked[c]) as u8, m.is_solid(v[0], v[1]) as u8, m.ground(v[0], v[1])));
        }
        print!("{}", out);
        return;
    }
    let (mut n, mut on_ground, mut solid_ground, mut blocked_ground, mut above, mut below, mut off_main) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    let mut worst: Vec<(f32, f32, f32, f32)> = Vec::new();
    let mut hist = [0u64; 5];
    let mut per: std::collections::BTreeMap<u32, u64> = std::collections::BTreeMap::new();
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let v: Vec<f32> = line.split_whitespace().filter_map(|s| s.parse().ok()).collect();
        if v.len() < 4 || v[3] < 0.5 {
            continue;
        }
        n += 1;
        let g = m.ground(v[0], v[1]);
        let dz = v[2] - g;
        if dz.abs() < 1.5 {
            on_ground += 1;
            let c = m.cell_of(v[0], v[1]);
            hist[m.wall_near[c].min(4) as usize] += 1;
            if m.is_solid(v[0], v[1]) {
                solid_ground += 1;
                if worst.len() < 12 {
                    worst.push((v[0], v[1], v[2], g));
                }
            }
            if m.blocked[c] {
                blocked_ground += 1;
            } else if m.comp[c] != m.main_comp {
                off_main += 1;
                *per.entry(m.comp[c]).or_insert(0u64) += 1;
            }
        } else if dz > 0.0 {
            above += 1;
        } else {
            below += 1;
            if worst.len() < 24 {
                worst.push((v[0], v[1], v[2], g));
            }
        }
    }
    println!("grounded samples {}: at my ground height {} ({:.1}%), above it (roofs/decks) {} ({:.1}%), below it {} ({:.2}%)", n, on_ground, 100.0 * on_ground as f64 / n as f64, above, 100.0 * above as f64 / n as f64, below, 100.0 * below as f64 / n as f64);
    println!("of the ground-level ones: inside my solid {} ({:.2}%), in a blocked (inflated) nav cell {} ({:.1}%), free but off the main component {} ({:.2}%)", solid_ground, 100.0 * solid_ground as f64 / on_ground as f64, blocked_ground, 100.0 * blocked_ground as f64 / on_ground as f64, off_main, 100.0 * off_main as f64 / on_ground as f64);
    println!("wall_near histogram (0 blocked .. 4 far): {:?}", hist);
    println!("examples (x, y, z, my ground): {:?}", worst);
    // components that real mechs stood in, and every sizeable component
    let mut size: std::collections::BTreeMap<u32, (u64, f32, f32, f32, f32, f32, f32)> = std::collections::BTreeMap::new();
    for c in 0..m.blocked.len() {
        if m.blocked[c] {
            continue;
        }
        let p = m.center(c);
        let e = size.entry(m.comp[c]).or_insert((0, f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY));
        e.0 += 1;
        e.1 = e.1.min(p[0]);
        e.2 = e.2.max(p[0]);
        e.3 = e.3.min(p[1]);
        e.4 = e.4.max(p[1]);
        e.5 = e.5.min(m.surf[c]);
        e.6 = e.6.max(m.surf[c]);
    }
    for (id, e) in &size {
        let samples = per.get(id).cloned().unwrap_or(0);
        if (e.0 >= 60 || samples > 0) && *id != m.main_comp {
            println!("comp {:5} cells {:6} x {:6.0}..{:6.0} y {:6.0}..{:6.0} surf {:6.1}..{:6.1} samples {}", id, e.0, e.1, e.2, e.3, e.4, e.5, e.6, samples);
        }
    }
    let free = m.blocked.iter().filter(|b| !**b).count();
    let main = m.comp.iter().filter(|c| **c == m.main_comp).count();
    println!("nav cells {} free {} main component {}", m.blocked.len(), free, main);
}
