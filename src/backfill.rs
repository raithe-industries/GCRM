// ------------------------------------------------------------
// RAiTHE INDUSTRIES INCORPORATED
// Copyright (c) 2026 All Rights Reserved.
//
// This file is part of a proprietary system. Unauthorized use,
// reproduction, or distribution is strictly prohibited.
// ------------------------------------------------------------

// src/backfill.rs — one-time event-archive theater backfill (GCRM v2 migration)
//
// Run with:  target/release/gcrm backfill
//
// Older archived events (logs/events_*.jsonl) were written before v2 added the
// `theater` field, so on restart they deserialize with theater = None, route to the
// "other" bucket, and the systemic layer reads low until the live window refills over
// hours. This migration assigns each untagged event its theater using the EXACT same
// theater_of() resolver the live processor uses (no drift), so the systemic index,
// theater ladder, and I&W board light up immediately on the next restart.
//
// Idempotent: events that already have a theater are left untouched. A one-time
// *.jsonl.bak backup is written before any file is overwritten.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::models::{theater_of, GeopoliticalEvent};

pub fn run() {
    let dir = "logs";
    let mut files: Vec<String> = Vec::new();
    match fs::read_dir(dir) {
        Ok(rd) => {
            for e in rd.flatten() {
                if let Some(n) = e.file_name().to_str() {
                    if n.starts_with("events_") && n.ends_with(".jsonl") {
                        files.push(format!("{dir}/{n}"));
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("backfill: cannot read {dir}/: {e}");
            return;
        }
    }
    files.sort();
    if files.is_empty() {
        println!("backfill: no logs/events_*.jsonl found — nothing to do.");
        return;
    }

    println!("backfill: scanning {} event archive file(s)…", files.len());
    println!("backfill: NOTE — run this while the GCRM service is STOPPED. The live\n\
              \x20         service appends to today's events file; rewriting it under a\n\
              \x20         running service can drop newly-appended events.\n");

    let mut total = 0usize;
    let mut filled = 0usize;
    let mut already = 0usize;
    let mut bad = 0usize;
    let mut dist: HashMap<&'static str, usize> = HashMap::new();

    for path in &files {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => { eprintln!("backfill: read {path}: {e} (skipped)"); continue; }
        };

        let mut out = String::with_capacity(text.len() + 1024);
        let mut changed = false;

        for line in text.lines() {
            if line.trim().is_empty() { continue; }
            match serde_json::from_str::<GeopoliticalEvent>(line) {
                Ok(mut ev) => {
                    total += 1;
                    if ev.theater.is_none() {
                        let t = theater_of(&ev.actor_ids, ev.region.as_deref());
                        *dist.entry(t.id()).or_insert(0) += 1;
                        ev.theater = Some(t.id().to_string());
                        filled += 1;
                        changed = true;
                    } else {
                        already += 1;
                    }
                    match serde_json::to_string(&ev) {
                        Ok(s)  => { out.push_str(&s); out.push('\n'); }
                        Err(_) => { out.push_str(line); out.push('\n'); }
                    }
                }
                // Keep any unparseable line verbatim so nothing is ever lost.
                Err(_) => { bad += 1; out.push_str(line); out.push('\n'); }
            }
        }

        if changed {
            let bak = format!("{path}.bak");
            if !Path::new(&bak).exists() {
                if let Err(e) = fs::copy(path, &bak) {
                    eprintln!("backfill: could not back up {path}: {e} — aborting this file");
                    continue;
                }
            }
            match fs::write(path, out) {
                Ok(())  => println!("  ✓ {path} — updated"),
                Err(e)  => eprintln!("  ✗ {path} — write failed: {e}"),
            }
        } else {
            println!("  · {path} — already tagged, skipped");
        }
    }

    println!("\nbackfill complete: {filled} tagged, {already} already had a theater, \
              {bad} unparseable, {total} total events.");
    if !dist.is_empty() {
        println!("theater distribution of newly-tagged events:");
        let mut d: Vec<(&'static str, usize)> = dist.into_iter().collect();
        d.sort_by(|a, b| b.1.cmp(&a.1));
        for (t, c) in d {
            println!("  {:<16} {}", t, c);
        }
    }
    println!("\nBackups written as *.jsonl.bak. Now START GCRM to load the tagged window:\n\
              \x20  sudo systemctl start gcrm.service");
}
