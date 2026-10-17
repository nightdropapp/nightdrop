//! Device-pass inspector: a real core over Tor, under a script's control, that records exactly what
//! a contact's phone delivers (MAINTENANCE.md §13).
//!
//!   cargo run -p nightdrop --features tor --example inspector -- <dir> <relay.onion>
//!
//! `<dir>` keeps the identity (state file + store key), so a restart is the same contact. It
//! approves every request, and writes each new message to `<dir>/log.jsonl`: text to
//! `<dir>/in/<id>.txt`, a picture as the app would draw it (`media_bytes`) to `<id>.jpg`, a video
//! as the app would open it (`media_to_file`) to `<id>.<ext>`, a preview to `<id>.thumb.jpg`.
//! Burn messages are left unopened until asked. Commands, one per line appended to `<dir>/cmd`:
//!   invite                       short-code invite -> <dir>/invite.txt
//!   text <contact> <words…>      send a text
//!   file <contact> <kind> <path> send image|video bytes as they are (`send_media`)
//!   open <contact> <msg_id>      open a burn message: decrypt to the player dir, mark viewed
//!   opened                       list the decrypted-media dir -> log
//!   quit

use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use nightdrop::api::NightdropCore;

fn log(dir: &Path, line: serde_json::Value) {
    println!("{line}");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("log.jsonl"))
        .unwrap();
    let _ = writeln!(f, "{line}");
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(args.get(1).expect("usage: inspector <dir> <relay.onion>"));
    let relay = args.get(2).cloned();
    std::fs::create_dir_all(dir.join("in"))?;
    let key_file = dir.join("store.key");
    if !key_file.exists() {
        let key: [u8; 32] = rand::random();
        std::fs::write(
            &key_file,
            base64::engine::general_purpose::STANDARD.encode(key),
        )?;
    }
    let key = std::fs::read_to_string(&key_file)?.trim().to_string();
    let state = dir.join("state.bin").to_string_lossy().into_owned();
    let core = NightdropCore::new_tor(
        Some(dir.join("tor").to_string_lossy().into_owned()),
        relay,
        Some(state),
        Some(key),
        None,
    )?;
    log(
        &dir,
        serde_json::json!({"event": "started", "address": core.address()}),
    );

    let mut seen: HashSet<String> = HashSet::new();
    let mut done_cmds = 0usize;
    let mut ready = false;
    loop {
        if !ready && core.onion_ready() {
            ready = true;
            log(&dir, serde_json::json!({"event": "onion_ready"}));
        }
        for req in core.incoming_requests() {
            let ok = core.authorize(&req.id, true).is_ok();
            log(
                &dir,
                serde_json::json!({"event": "authorized", "contact": req.id, "ok": ok}),
            );
        }
        for c in core.contacts() {
            for m in core.messages(&c.id) {
                // Attachments are known by their transfer id, texts by their message id.
                let id = [&m.msg_id, &m.transfer_id, &m.media_id]
                    .into_iter()
                    .find(|s| !s.is_empty())
                    .cloned()
                    .unwrap_or_default();
                if id.is_empty() || !seen.insert(format!("{}:{}", c.id, id)) {
                    continue;
                }
                let mut saved = Vec::new();
                if !m.text.is_empty() {
                    std::fs::write(dir.join("in").join(format!("{id}.txt")), &m.text)?;
                }
                if !m.media_id.is_empty() && m.burn_secs == 0 && !m.from_me {
                    saved.push(save_media(&core, &dir, &id, &m.kind, &m.media_id));
                }
                if !m.thumb_id.is_empty() && !m.from_me {
                    let r = core.media_bytes(&m.thumb_id).map(|b| {
                        let p = dir.join("in").join(format!("{id}.thumb.jpg"));
                        std::fs::write(&p, b).map(|_| p.display().to_string())
                    });
                    saved.push(format!("{r:?}"));
                }
                log(
                    &dir,
                    serde_json::json!({"event": "message", "contact": c.id, "msg_id": id,
                    "from_me": m.from_me, "system": m.system, "kind": m.kind, "mime": m.mime,
                    "text_bytes": m.text.len(), "text": m.text.chars().take(80).collect::<String>(),
                    "media_size": m.media_size, "burn_secs": m.burn_secs, "delivery": m.delivery,
                    "saved": saved}),
                );
            }
        }
        // Commands.
        let cmds = std::fs::read_to_string(dir.join("cmd")).unwrap_or_default();
        let lines: Vec<&str> = cmds.lines().collect();
        for line in lines.iter().skip(done_cmds) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let r: anyhow::Result<serde_json::Value> = match parts.as_slice() {
                ["invite"] => core.create_short_code_invite().map(|code| {
                    let _ = std::fs::write(dir.join("invite.txt"), &code);
                    serde_json::json!(code)
                }),
                ["text", contact, words @ ..] => core
                    .send_message(contact, &words.join(" "))
                    .map(|_| serde_json::json!("sent")),
                ["file", contact, kind, path] => std::fs::read(path)
                    .map_err(Into::into)
                    .and_then(|data| {
                        let mime = if *kind == "video" {
                            "video/mp4"
                        } else {
                            "image/jpeg"
                        };
                        core.send_media(contact, data, mime.into(), kind.to_string(), vec![])
                    })
                    .map(|_| serde_json::json!("sent")),
                ["open", contact, msg_id] => {
                    let m = core
                        .messages(contact)
                        .into_iter()
                        .find(|m| m.msg_id == *msg_id || m.transfer_id == *msg_id);
                    match m {
                        Some(m) => {
                            let saved = save_media(&core, &dir, msg_id, &m.kind, &m.media_id);
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)?
                                .as_secs();
                            core.mark_burn_viewed(contact, msg_id, now).map(
                                |_| serde_json::json!({"opened": saved, "burn_secs": m.burn_secs}),
                            )
                        }
                        None => Err(anyhow::anyhow!("no such message")),
                    }
                }
                ["opened"] => Ok(serde_json::json!(list_dir(&dir.join("nightdrop-open")))),
                ["quit"] => {
                    log(&dir, serde_json::json!({"event": "quit"}));
                    core.shutdown();
                    return Ok(());
                }
                _ => Err(anyhow::anyhow!("unknown command")),
            };
            log(
                &dir,
                serde_json::json!({"event": "cmd", "cmd": line,
                "result": r.map_err(|e| e.to_string())}),
            );
        }
        done_cmds = lines.len();
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// A received attachment as the app would get it: a picture through `media_bytes` (the gated
/// call the app draws from), a video through `media_to_file` (the player copy).
fn save_media(core: &NightdropCore, dir: &Path, id: &str, kind: &str, media_id: &str) -> String {
    let out = dir.join("in");
    if kind == "video" {
        match core.media_to_file(media_id, String::new()) {
            Ok(p) => {
                let ext = Path::new(&p)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("bin");
                let dest = out.join(format!("{id}.{ext}"));
                std::fs::copy(&p, &dest)
                    .map_or_else(|e| e.to_string(), |_| dest.display().to_string())
            }
            Err(e) => format!("refused: {e}"),
        }
    } else {
        match core.media_bytes(media_id) {
            Ok(b) => {
                let dest = out.join(format!("{id}.jpg"));
                std::fs::write(&dest, b)
                    .map_or_else(|e| e.to_string(), |_| dest.display().to_string())
            }
            Err(e) => format!("refused: {e}"),
        }
    }
}

fn list_dir(p: &Path) -> Vec<String> {
    std::fs::read_dir(p)
        .map(|r| {
            r.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}
