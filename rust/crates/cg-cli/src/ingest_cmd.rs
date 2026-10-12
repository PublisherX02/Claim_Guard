//! `cg ingest <file-or-folder> --output claims.jsonl [--quarantine q.jsonl] [--report r.json] [--items items.jsonl] [--format F]`
//!
//! Converts whatever a claim arrives in (normalized JSON / JSONL, FHIR bundles, a folder with the five CSV tables) into normalized claims. A record
//! that cannot be accepted is written to the quarantine file with its reason; it never stops the run and is never dropped. Exit code 0 even when some
//! records were quarantined (the summary says how many); 2 when the input itself cannot be read.

use cg_ingest::{ingest_csv_files, ingest_json_bytes, summarize, Ingested, CSV_FOLDER, FHIR, NORMALIZED};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn read_folder(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut files = BTreeMap::new();
    for t in cg_ingest::csvfmt::TABLES {
        let p = dir.join(format!("{t}.csv"));
        if let Ok(b) = std::fs::read(&p) {
            files.insert(t.to_string(), b);
        }
    }
    Ok(files)
}

pub fn ingest(args: &[String]) -> i32 {
    let (Some(input), Some(output)) = (args.first(), arg(args, "--output")) else {
        eprintln!("usage: cg ingest <file-or-folder> --output claims.jsonl [--quarantine q.jsonl] [--report r.json] [--items items.jsonl] [--format normalized_json|fhir_bundle|csv_folder]");
        return 2;
    };
    let path = Path::new(input);
    let forced = arg(args, "--format");
    let items: Vec<Ingested> = if path.is_dir() {
        if !path.join("claims.csv").exists() {
            eprintln!("error: {input} is a directory without claims.csv");
            return 2;
        }
        match read_folder(path) {
            Ok(files) => ingest_csv_files(&files, path.file_name().and_then(|n| n.to_str()).unwrap_or("folder")),
            Err(e) => {
                eprintln!("error: {e}");
                return 2;
            }
        }
    } else {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("error: cannot read {input}: {e}");
                return 2;
            }
        };
        let fmt = match forced.as_deref() {
            Some(f @ (NORMALIZED | FHIR)) => Some(f),
            Some(CSV_FOLDER) => {
                eprintln!("error: a csv_folder is a directory");
                return 2;
            }
            Some(other) => {
                eprintln!("error: unknown format {other}");
                return 2;
            }
            None => None,
        };
        ingest_json_bytes(&bytes, path.file_name().and_then(|n| n.to_str()).unwrap_or("input"), fmt)
    };
    let accepted: String = items.iter().filter_map(|i| i.claim.as_ref()).map(|c| format!("{c}\n")).collect();
    if let Err(e) = std::fs::write(&output, accepted) {
        eprintln!("error: cannot write {output}: {e}");
        return 2;
    }
    if let Some(q) = arg(args, "--quarantine") {
        let lines: String = items.iter().filter(|i| !i.accepted()).map(|i| format!("{}\n", json!({"source_ref": i.source_ref, "error": i.error, "raw": i.raw}))).collect();
        let _ = std::fs::write(q, lines);
    }
    if let Some(p) = arg(args, "--items") {
        let lines: String = items.iter().map(|i| format!("{}\n", json!({"source_ref": i.source_ref, "accepted": i.accepted(), "error": i.error, "report": i.report}))).collect();
        let _ = std::fs::write(p, lines);
    }
    let summary: Value = summarize(&items);
    if let Some(r) = arg(args, "--report") {
        let _ = std::fs::write(r, serde_json::to_string_pretty(&summary).unwrap_or_default());
    }
    println!("{}", serde_json::to_string_pretty(&summary).unwrap_or_default());
    0
}

/// `cg ingest-doc <file>... --output drafts.jsonl [--root DIR]`: gate -> reader sidecar -> extraction, one JSON line per file:
/// {"file", "sha256", "refused": {code, reason}} | {"file", "error"} | {"file", "sha256", "reader": {engine, pages: [{page, source}]}, "draft": {...}}.
/// The reader is configured by DOCUMENT_READER_URL / _TOKEN / _ENGINE / _LANGS in the environment.
pub fn ingest_doc(args: &[String]) -> i32 {
    use cg_ingest::{document, extract, gate};
    let Some(output) = arg(args, "--output") else {
        eprintln!("usage: cg ingest-doc <file>... --output drafts.jsonl [--root DIR]");
        return 2;
    };
    let files: Vec<&String> = {
        let mut v = vec![];
        let mut i = 0;
        while i < args.len() {
            if args[i].starts_with("--") {
                i += 2;
            } else {
                v.push(&args[i]);
                i += 1;
            }
        }
        v
    };
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let cfg = match document::ReaderConfig::from_env(&env) {
        Ok(Some(c)) => c,
        Ok(None) => {
            eprintln!("error: no document reader is configured (DOCUMENT_READER_URL and DOCUMENT_READER_TOKEN)");
            return 2;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let root = std::path::PathBuf::from(arg(args, "--root").or_else(|| env.get("CLAIMGUARD_ROOT").cloned()).unwrap_or_else(|| ".".into()));
    let catalog = extract::Catalog::load(&root);
    let mut out = String::new();
    for f in files {
        let name = Path::new(f).file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
        let bytes = match std::fs::read(f) {
            Ok(b) => b,
            Err(e) => {
                out.push_str(&format!("{}\n", json!({"file": name, "error": format!("cannot read: {e}")})));
                continue;
            }
        };
        let line = match gate::inspect(&bytes) {
            Err(r) => json!({"file": name, "refused": {"code": r.code, "reason": r.reason}}),
            Ok(i) => match document::read_document(&cfg, &bytes, i.kind.mime()) {
                Err(e) => json!({"file": name, "sha256": i.sha256, "error": e.to_string(), "refused_by_reader": matches!(e, document::ReadError::Refused(_))}),
                Ok(read) => {
                    let draft = extract::extract(&read.pages, &i.sha256, &catalog);
                    json!({"file": name, "sha256": i.sha256, "reader": {"engine": read.engine, "pages": read.pages.iter().map(|p| json!({"page": p.page, "source": p.source})).collect::<Vec<_>>()}, "draft": draft.to_json()})
                }
            },
        };
        out.push_str(&format!("{line}\n"));
    }
    if let Err(e) = std::fs::write(&output, out) {
        eprintln!("error: cannot write {output}: {e}");
        return 2;
    }
    0
}
