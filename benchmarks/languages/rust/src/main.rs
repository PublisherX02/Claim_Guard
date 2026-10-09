mod rules;

use rayon::prelude::*;
use rules::{Claim, Pack};
use std::sync::Arc;
use std::time::Instant;

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn arg(args: &[String], name: &str, default: &str) -> String {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned().unwrap_or_else(|| default.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let corpus = arg(&args, "-corpus", "");
    let threads: usize = arg(&args, "-threads", &std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).to_string()).parse().unwrap();
    let repeat: usize = arg(&args, "-repeat", "5").parse().unwrap();
    let serve = arg(&args, "-serve", "");

    let pol = std::fs::read_to_string(format!("{corpus}/pack/policies.json")).unwrap();
    let svc = std::fs::read_to_string(format!("{corpus}/pack/services.json")).unwrap();
    let pack = Pack::new(&pol, &svc).unwrap();
    if !serve.is_empty() {
        serve_http(pack, &serve);
        return;
    }

    let t = Instant::now();
    let raw = std::fs::read_to_string(format!("{corpus}/claims.jsonl")).unwrap();
    let read_ms = t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    let claims: Vec<Claim> = raw.lines().filter(|l| !l.is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect();
    let parse_ms = t.elapsed().as_secs_f64() * 1000.0;

    let out: Vec<[u8; 15]> = claims.iter().map(|c| pack.evaluate(c)).collect();
    let expected = std::fs::read_to_string(format!("{corpus}/expected.txt")).unwrap();
    let mismatches = expected.lines().zip(out.iter()).filter(|(e, o)| e.as_bytes() != &o[..]).count();

    let tp = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
    let (mut one, mut many) = (vec![], vec![]);
    for _ in 0..repeat {
        let t = Instant::now();
        let r: Vec<[u8; 15]> = claims.iter().map(|c| pack.evaluate(c)).collect();
        std::hint::black_box(&r);
        one.push(t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        let r: Vec<[u8; 15]> = tp.install(|| claims.par_iter().map(|c| pack.evaluate(c)).collect());
        std::hint::black_box(&r);
        many.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    println!(
        "{}",
        serde_json::json!({"lang":"rust","version":"rustc 1.99","claims":claims.len(),"mismatches":mismatches,"threads":threads,
            "read_ms":read_ms,"parse_ms":parse_ms,"eval_1t_ms":median(one),"eval_mt_ms":median(many)})
    );
}

fn serve_http(pack: Pack, addr: &str) {
    use axum::{body::Bytes, http::StatusCode, routing::{get, post}, Router};
    let pack = Arc::new(pack);
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async move {
        let p = pack.clone();
        let app = Router::new()
            .route(
                "/evaluate",
                post(move |body: Bytes| {
                    let p = p.clone();
                    async move {
                        if body.len() > (1 << 20) {
                            return Err(StatusCode::PAYLOAD_TOO_LARGE);
                        }
                        let c: Claim = serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
                        let out = p.evaluate(&c);
                        Ok(format!("{{\"claim_id\":\"{}\",\"statuses\":\"{}\"}}", c.claim_id, String::from_utf8_lossy(&out)))
                    }
                }),
            )
            .route("/healthz", get(|| async { "ok" }));
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        axum::serve(listener, app).await.unwrap();
    });
}
