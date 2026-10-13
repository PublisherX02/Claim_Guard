//! Importing claims from files: the preview step. A file goes in; what comes out is what the system WOULD accept, field by field, with the reasons for
//! everything it would not, and nothing is queued. The person looks, corrects what the draft got wrong, and only then submits through the ordinary
//! submit route, which validates again.
//!
//!   POST /api/v1/queue/import/file   the raw bytes of one file, `X-File-Name` for display. The kind is read from the bytes, never from the name:
//!                                    a PDF, PNG or JPEG is a document (gate, reader sidecar, extraction into a draft); anything else is read as
//!                                    normalized JSON / JSONL or FHIR bundles, one record at a time, each accepted or quarantined with its reason.
//!   POST /api/v1/queue/import/csv    {"claims": text, "lines": text, "coverage": text, "authorizations": text, "attachments": text}: the five tables
//!                                    of the relational export.
//!
//! Both need `routing.manage` (the permission that submits claims) and are recorded in the security log with the file's hash, size and outcome.

use crate::{Api, ApiError, ApiRequest, ApiResponse, R};
use cg_ingest::{document, extract, gate, ingest_csv_files, ingest_json_bytes, summarize, Ingested};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

pub const MAX_RECORDS: usize = 500;
const MAX_DOCUMENT_JOBS: usize = 2;

pub struct ImportParts {
    pub reader: Option<document::ReaderConfig>,
    pub catalog: extract::Catalog,
}

#[derive(Default)]
pub struct DocumentSlots(AtomicUsize);

struct Slot<'a>(&'a AtomicUsize);

impl DocumentSlots {
    fn take(&self) -> Option<Slot<'_>> {
        if self.0.fetch_add(1, Ordering::SeqCst) >= MAX_DOCUMENT_JOBS {
            self.0.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Slot(&self.0))
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn display_name(req: &ApiRequest) -> String {
    let raw = req.headers.get("x-file-name").map(String::as_str).unwrap_or("upload");
    let clean: String = raw.chars().filter(|c| c.is_alphanumeric() || " ._-()".contains(*c)).take(80).collect();
    if clean.trim().is_empty() { "upload".into() } else { clean }
}

fn item_json(i: &Ingested) -> Value {
    let warnings = i.report["warnings"].as_array().cloned().unwrap_or_default();
    json!({"ref": i.source_ref, "accepted": i.accepted(), "claim": i.claim, "error": i.error, "warnings": warnings})
}

impl Api {
    fn log_import(&self, req: &ApiRequest, p: &cg_access::Principal, fields: Value) -> Result<(), ApiError> {
        let mut f = fields;
        f["actor"] = json!(p.badge());
        f["command"] = json!("import_preview");
        self.access.record("queue_admin", f).map_err(|_| self.serr_unavailable(req))
    }

    fn serr_unavailable(&self, _req: &ApiRequest) -> ApiError {
        ApiError::new(503, "unavailable")
    }

    pub(crate) fn import_file(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "routing.manage")?;
        if req.body.is_empty() {
            return Err(ApiError::new(422, "invalid_request").with("detail", json!("the file is empty")));
        }
        let name = display_name(req);
        match gate::inspect(&req.body) {
            Ok(info) => self.import_document(req, &p, info, &name),
            Err(r) if r.code == "unsupported_type" => self.import_records(req, &p, &name),
            Err(r) => {
                self.log_import(req, &p, json!({"kind": "refused", "code": r.code, "bytes": req.body.len()}))?;
                Err(ApiError::new(422, "refused").with("code", json!(r.code)).with("reason", json!(r.reason)))
            }
        }
    }

    fn import_records(&self, req: &ApiRequest, p: &cg_access::Principal, name: &str) -> R {
        let items = ingest_json_bytes(&req.body, name, None);
        if items.len() > MAX_RECORDS {
            return Err(ApiError::new(422, "too_many_records").with("detail", json!(format!("{} records; at most {MAX_RECORDS} can be read at once", items.len()))));
        }
        let summary = summarize(&items);
        let sha = cg_ingest_hash(&req.body);
        self.log_import(req, p, json!({"kind": "records", "format": summary["source_format"], "sha256": sha, "bytes": req.body.len(), "accepted": summary["accepted"], "quarantined": summary["quarantined"]}))?;
        Ok(ApiResponse::json(200, &json!({"kind": "records", "source": {"name": name, "sha256": sha, "bytes": req.body.len(), "format": summary["source_format"]},
                                           "items": items.iter().map(item_json).collect::<Vec<_>>(), "summary": summary})))
    }

    fn import_document(&self, req: &ApiRequest, p: &cg_access::Principal, info: gate::Inspected, name: &str) -> R {
        let Some(parts) = self.import.as_ref() else { return Err(ApiError::new(503, "document_reader_not_configured")) };
        let Some(cfg) = parts.reader.as_ref() else {
            return Err(ApiError::new(503, "document_reader_not_configured").with("detail", json!("this server has no document reader; set DOCUMENT_READER_URL and DOCUMENT_READER_TOKEN (docs/45)")));
        };
        let Some(_slot) = self.doc_slots.take() else { return Err(ApiError::new(429, "busy").with("detail", json!("two documents are being read; try again in a moment"))) };
        let read = match document::read_document(cfg, &req.body, info.kind.mime()) {
            Ok(r) => r,
            Err(document::ReadError::Refused(m)) => {
                self.log_import(req, p, json!({"kind": "document_refused", "sha256": info.sha256, "bytes": info.bytes, "reason": m.chars().take(200).collect::<String>()}))?;
                return Err(ApiError::new(422, "refused").with("code", json!("reader_refused")).with("reason", json!(m)));
            }
            Err(document::ReadError::Unavailable(m)) => return Err(ApiError::new(503, "document_reader_unavailable").with("detail", json!(m))),
        };
        let draft = extract::extract(&read.pages, &info.sha256, &parts.catalog);
        self.log_import(req, p, json!({"kind": "document", "sha256": info.sha256, "bytes": info.bytes, "draft": draft.kind, "valid": draft.valid, "engine": read.engine}))?;
        Ok(ApiResponse::json(200, &json!({
            "kind": "document",
            "source": {"name": name, "sha256": info.sha256, "bytes": info.bytes, "mime": info.kind.mime(), "dimensions": info.dims},
            "reader": {"engine": read.engine, "pages": read.pages.iter().map(|pg| json!({"page": pg.page, "source": pg.source, "second_reading": pg.second_text.is_some()})).collect::<Vec<_>>()},
            "draft": draft.to_json(),
        })))
    }

    /// Is this (edited) claim acceptable to the transport contract? The same check every claim passes at intake; it changes nothing.
    pub(crate) fn import_validate(&self, req: &ApiRequest) -> R {
        self.need(req, "routing.manage")?;
        let body: Value = serde_json::from_slice(&req.body).map_err(|_| ApiError::new(422, "invalid_request"))?;
        let claim = body.get("claim").filter(|c| c.is_object()).ok_or_else(|| ApiError::new(422, "invalid_request"))?;
        Ok(match cg_core::transport::validate(claim) {
            Ok(()) => ApiResponse::json(200, &json!({"valid": true, "error": null})),
            Err(e) => ApiResponse::json(200, &json!({"valid": false, "error": e})),
        })
    }

    pub(crate) fn import_csv(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "routing.manage")?;
        let body: Value = serde_json::from_slice(&req.body).map_err(|_| ApiError::new(422, "invalid_request"))?;
        let obj = body.as_object().ok_or_else(|| ApiError::new(422, "invalid_request"))?;
        let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for t in cg_ingest::csvfmt::TABLES {
            let text = obj.get(t).and_then(Value::as_str).ok_or_else(|| ApiError::new(422, "invalid_request").with("detail", json!(format!("the {t} table is missing"))))?;
            files.insert(t.to_string(), text.as_bytes().to_vec());
        }
        if obj.keys().any(|k| !cg_ingest::csvfmt::TABLES.contains(&k.as_str())) {
            return Err(ApiError::new(422, "invalid_request"));
        }
        let items = ingest_csv_files(&files, "upload");
        if items.len() > MAX_RECORDS {
            return Err(ApiError::new(422, "too_many_records").with("detail", json!(format!("{} claims; at most {MAX_RECORDS} can be read at once", items.len()))));
        }
        let summary = summarize(&items);
        let total: usize = files.values().map(Vec::len).sum();
        let mut all = Vec::with_capacity(total);
        for t in cg_ingest::csvfmt::TABLES {
            all.extend_from_slice(&files[t]);
        }
        let sha = cg_ingest_hash(&all);
        self.log_import(req, &p, json!({"kind": "csv", "sha256": sha, "bytes": total, "accepted": summary["accepted"], "quarantined": summary["quarantined"]}))?;
        Ok(ApiResponse::json(200, &json!({"kind": "records", "source": {"name": "csv tables", "sha256": sha, "bytes": total, "format": "csv_folder"},
                                           "items": items.iter().map(item_json).collect::<Vec<_>>(), "summary": summary})))
    }
}

fn cg_ingest_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}
