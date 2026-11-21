//! Dev tool: run the brain over a recorded observation stream (it cannot react, so only the
//! per-tick decisions are meaningful) and print what it decides.  feed <stream.jsonl> <map_dir>
use mechbot::brain::Brain;
use serde_json::Value;
use std::io::BufRead;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let f = std::fs::File::open(&args[1]).expect("stream");
    let mut brain: Option<Brain> = None;
    for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
        let mut v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match v["type"].as_str() {
            Some("hello") => {
                v["map"]["dir"] = Value::String(args[2].clone());
                brain = Some(Brain::new(&v));
            }
            Some("obs") => {
                let b = brain.as_mut().unwrap();
                let a = b.decide(&v);
                let me = &v["self"];
                let near: Vec<String> = v["mechs"].as_array().unwrap().iter().map(|m| format!("{}{}@{:.0}m hp{:.0}", if m["kind"] == "npc" { "n" } else { "m" }, m["id"], ((m["pos"][0].as_f64().unwrap() - me["pos"][0].as_f64().unwrap()).powi(2) + (m["pos"][1].as_f64().unwrap() - me["pos"][1].as_f64().unwrap()).powi(2)).sqrt(), m["hp"].as_f64().unwrap_or(0.0))).collect();
                println!("t={:5.1} hp={:4.0} {:6} mv=({:5.2},{:5.2}) aim=({:5.2},{:5.2}) real_aim=({:5.2},{:5.2}) fire={} boost={} jump={} take={} sees=[{}] ev={}", v["t"].as_f64().unwrap(), me["hp"].as_f64().unwrap(), b.label, a["move"][0].as_f64().unwrap(), a["move"][1].as_f64().unwrap(), a["aim"][0].as_f64().unwrap(), a["aim"][1].as_f64().unwrap(), me["aim"][0].as_f64().unwrap(), me["aim"][1].as_f64().unwrap(), a["fire"], a["boost"], a["jump"], a["take"], near.join(" "), v["events"].as_array().map_or(0, |e| e.len()));
            }
            _ => {}
        }
    }
}
