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
