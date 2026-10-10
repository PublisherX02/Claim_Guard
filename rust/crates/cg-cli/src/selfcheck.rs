//! `cg selfcheck`: what the process can and cannot do in the place it runs, printed as one JSON object. For deployment checks in a container
//! that has no shell: who am I, which places can I write, which capabilities do I hold, can I gain privileges, which tools are present.

use serde_json::{json, Value};
use std::path::Path;

fn can_write(dir: &str) -> bool {
    let p = Path::new(dir).join(format!(".cg_selfcheck_{}", std::process::id()));
    let ok = std::fs::write(&p, b"x").is_ok();
    let _ = std::fs::remove_file(&p);
    ok
}

fn status_field(name: &str) -> Option<String> {
    std::fs::read_to_string("/proc/self/status").ok()?.lines().find_map(|l| l.strip_prefix(&format!("{name}:")).map(|v| v.trim().to_string()))
}

pub fn run() -> i32 {
    let uid = status_field("Uid").and_then(|v| v.split_whitespace().next().map(str::to_string));
    let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
    let tools: Vec<&str> = ["sh", "bash", "curl", "wget", "python", "python3", "gcc", "cc", "make", "nc", "ssh", "git", "apt", "apt-get", "perl"]
        .into_iter()
        .filter(|t| ["/bin", "/usr/bin", "/usr/local/bin", "/sbin", "/usr/sbin"].iter().any(|d| Path::new(d).join(t).exists()))
        .collect();
    let out: Value = json!({
        "uid": uid,
        "cap_effective": status_field("CapEff"),
        "cap_bounding": status_field("CapBnd"),
        "no_new_privs": status_field("NoNewPrivs"),
        "writable": {"/": can_write("/"), "/app": can_write("/app"), "/usr/local/bin": can_write("/usr/local/bin"), "/tmp": can_write("/tmp"), "/data": can_write("/data"), "/mirror": can_write("/mirror")},
        "mirror_is_a_mount": mounts.lines().any(|l| l.split_whitespace().nth(1) == Some("/mirror")),
        "tools_present": tools,
        "env_secret_names": std::env::vars().map(|(k, _)| k).filter(|k| ["SECRET", "KEY", "PASSWORD", "TOKEN"].iter().any(|w| k.contains(w))).collect::<Vec<_>>(),
    });
    println!("{out}");
    0
}
