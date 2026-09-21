//! Throwaway P3 Task 0 protocol spike (NOT production code — `src/` must never reference it).
//!
//! Answers the six questions of the P3 plan about the `mysql` crate (28.0.2, `binlog`
//! feature) binlog-dump surface against a real mysql:8.0 container.
//!
//! Usage:
//!   cargo run --example repl_spike -- dump   <url> <secs> <hb_secs|0> [file pos]
//!   cargo run --example repl_spike -- auth   <url_sha2> <url_native>
//!
//! Reconstructed event bytes are written to /tmp/p3spike_events.hex as
//! `HEX <start_pos> <size> <crc_alg> <hex>` lines for byte-for-byte comparison
//! with the real binlog files on disk.

use std::io::Write;
use std::sync::mpsc::sync_channel;
use std::thread::spawn;
use std::time::{Duration, Instant};

use mysql::prelude::Queryable;
use mysql::{BinlogRequest, BinlogStream, Conn, Opts, Row};

const HEX_PATH: &str = "/tmp/p3spike_events.hex";
const VERSION4: mysql::binlog::BinlogVersion = mysql::binlog::BinlogVersion::Version4;

fn hexdump(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn one_row(conn: &mut Conn, q: &str) -> Option<Row> {
    let mut rs = conn.query_iter(q).ok()?;
    match rs.next() {
        Some(Ok(row)) => Some(row),
        _ => None,
    }
}

/// Current (file, pos) from the master, via SHOW MASTER STATUS.
fn master_status(conn: &mut Conn) -> (String, u64) {
    let row = one_row(conn, "SHOW MASTER STATUS").expect("SHOW MASTER STATUS failed");
    (
        row.get::<String, _>(0).unwrap_or_default(),
        row.get::<u64, _>(1).unwrap_or(4),
    )
}

fn on_event(seq: u64, cur_pos: u64, event: &mysql::binlog::events::Event, w: &mut dyn Write) {
    let h = event.header();
    let tname = match h.event_type() {
        Ok(t) => format!("{t:?}"),
        Err(_) => format!("UNKNOWN(0x{:02x})", h.event_type_raw()),
    };
    let mut rebuilt = Vec::new();
    let rebuild_ok = event.write(VERSION4, &mut rebuilt).is_ok();
    let crc = event.checksum();
    let data_len = event.data().len();

    // Stream-head fake rotate (dump-thread synthetic "connection event"): ROTATE
    // with the ARTIFICIAL header flag (0x20), ts=0 and *header* log_pos=0
    // (measured). The *payload* `position` carries the requested start pos
    // (measured: 4 and 292528), so `RotateEvent::is_fake()` (payload==0) is NOT
    // a valid discriminator. Note: the EOF rotation frame (mid-stream, seq>0) is
    // ALSO synthetic (ts=0/artificial/header log_pos=0, payload=4, CRC present,
    // file truncated to 180B on close) — measured on-disk rotate bytes: none —
    // but it announces a real file switch, so it stays labeled ROTATE.
    let rotate_payload_pos = match event.read_data() {
        Ok(Some(mysql::binlog::events::EventData::RotateEvent(re))) => Some(re.position()),
        _ => None,
    };
    let fake_rotate = rotate_payload_pos.is_some()
        && seq == 0
        && (h.flags_raw() & 0x0020) != 0
        && h.timestamp() == 0
        && h.log_pos() == 0;
    let rot_info = rotate_payload_pos
        .map(|p| format!(" rotate_payload_pos={p}"))
        .unwrap_or_default();
    let kind = if fake_rotate {
        "FAKE_ROTATE"
    } else if tname.contains("HEARTBEAT") {
        "HEARTBEAT"
    } else if tname.contains("FORMAT_DESCRIPTION") {
        "FDE"
    } else if tname.contains("ROTATE") {
        "ROTATE"
    } else {
        "EVENT"
    };

    let _ = writeln!(
        w,
        "seq={} {} type={} hdr_flags=0x{:04x} ts={} srv={} start_pos={} size={} next_log_pos={} \
         data_len={} crc={} rebuilt_len={} rebuilt_eq_size={}{rot_info}",
        seq,
        kind,
        tname,
        h.flags_raw(),
        h.timestamp(),
        h.server_id(),
        cur_pos,
        h.event_size(),
        h.log_pos(),
        data_len,
        crc.map_or("-".to_string(), |c| format!(
            "{}:{}",
            if event.footer().get_checksum_enabled() {
                "on"
            } else {
                "off"
            },
            hexdump(&c)
        )),
        rebuilt.len(),
        rebuild_ok && rebuilt.len() == h.event_size() as usize,
    );

    // Persist reconstructed bytes for the external disk comparison (Q1).
    // FDE is excluded on purpose: its BINLOG_IN_USE flag differs on disk after close.
    if rebuild_ok
        && rebuilt.len() == h.event_size() as usize
        && !tname.contains("FORMAT_DESCRIPTION")
        && let Ok(Some(alg)) = event.footer().get_checksum_alg()
    {
        let _ = writeln!(
            w,
            "HEX {} {} {} {}",
            cur_pos,
            rebuilt.len(),
            alg as u8,
            hexdump(&rebuilt)
        );
    }
}

/// Iterates the stream with 1s recv_timeout; never panics; returns after `secs` or
/// after the stream ends/errs (Err form printed).
fn pump_stream(stream: BinlogStream, secs: u64, cap: usize) {
    let (tx, rx) = sync_channel::<mysql::Result<mysql::binlog::events::Event>>(1);
    spawn(move || {
        for ev in stream {
            if tx.send(ev).is_err() {
                break;
            }
        }
    });
    let t0 = Instant::now();
    let mut seq = 0u64;
    let mut prev_next: Option<u32> = None;
    let mut file = std::fs::File::create(HEX_PATH).expect("create hex out");
    let mut w = std::io::BufWriter::new(&mut file);
    let mut idle_ticks = 0u32;
    while t0.elapsed() < Duration::from_secs(secs) && (seq as usize) < cap {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Ok(event)) => {
                let cur_pos = match prev_next {
                    Some(p) => u64::from(p),
                    None => 0,
                };
                let h = event.header();
                on_event(seq, cur_pos, &event, &mut w);
                prev_next = Some(h.log_pos());
                seq += 1;
                idle_ticks = 0;
            }
            Ok(Err(e)) => {
                let _ = writeln!(w, "STREAM_ERR {e:?}");
                println!("STREAM_ERR {e:?}");
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(more) => println!("after_err_another_event={more:?}"),
                    // Deterministic, discriminating outcomes (measured: hard
                    // disconnect reaches Disconnected — iterator None, sender
                    // dropped by the finished spawn thread):
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        println!("after_err_probe=timeout_still_open_no_frame_within_100ms");
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        println!("after_err_probe=disconnected_stream_poisoned_next_is_none");
                    }
                }
                break;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                idle_ticks += 1;
                if idle_ticks % 5 == 1 {
                    println!("idle seq={seq} elapsed_s={}", t0.elapsed().as_secs());
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                println!("STREAM_ENDED (iterator returned None / sender dropped)");
                break;
            }
        }
    }
    let _ = w.flush();
    println!("pump_done events={seq} hex_lines_written_to={HEX_PATH}");
}

fn cmd_dump(url: &str, secs: u64, hb_secs: u64, file: Option<&str>, pos: Option<u64>) {
    let mut conn = match Conn::new(url) {
        Ok(c) => c,
        Err(e) => {
            println!("CONNECT_ERR {e:?}");
            return;
        }
    };
    let (mfile, mpos) = master_status(&mut conn);
    println!(
        "server_version={:?} master_status file={mfile} pos={mpos}",
        conn.server_version()
    );
    if hb_secs > 0 {
        match conn.query_drop(format!(
            "SET @master_heartbeat_period = {}",
            hb_secs * 1_000_000_000
        )) {
            Ok(()) => {
                let back = one_row(&mut conn, "SELECT @master_heartbeat_period")
                    .and_then(|r| r.get::<String, _>(0));
                println!("heartbeat_set ok readback={back:?} ns");
            }
            Err(e) => println!("heartbeat_set ERR {e:?}"),
        }
    }
    let req = match (file, pos) {
        (Some(f), Some(p)) => BinlogRequest::new(9999)
            .with_filename(f.as_bytes().to_vec())
            .with_pos(p),
        _ => BinlogRequest::new(9999), // empty filename -> master starts at FIRST known binlog
    };
    println!(
        "request server_id={} file={:?} pos={} flags={:?}",
        req.server_id(),
        String::from_utf8_lossy(req.filename()),
        req.pos(),
        req.flags()
    );
    match conn.get_binlog_stream(req) {
        Ok(stream) => pump_stream(stream, secs, 500),
        Err(e) => println!("GET_BINLOG_STREAM_ERR {e:?}"),
    }
}

fn try_conn(label: &str, url: &str) {
    match Conn::new(url) {
        Ok(mut conn) => {
            let probe = one_row(&mut conn, "SELECT 1, CURRENT_USER()")
                .and_then(|r| Some((r.get::<String, _>(0)?, r.get::<String, _>(1)?)));
            match probe {
                Some((v, who)) => println!("{label}: OK (1={v} as {who})"),
                None => println!("{label}: CONNECTED but probe failed"),
            }
        }
        Err(e) => println!("{label}: CONNECT_ERR {e:?}"),
    }
}

fn cmd_auth(url_sha2: &str, url_native: &str) {
    try_conn("auth_caching_sha2_over_tcp", url_sha2);
    try_conn("auth_native_over_tcp", url_native);
    // native user must also be able to dump
    let mut c = match Conn::new(url_native) {
        Ok(c) => c,
        Err(e) => {
            println!("auth_native_dump: CONNECT_ERR {e:?}");
            return;
        }
    };
    let (f, p) = master_status(&mut c);
    let dump_file = f.clone();
    match c.get_binlog_stream(
        BinlogRequest::new(9998)
            .with_filename(dump_file.into_bytes())
            .with_pos(p),
    ) {
        Ok(s) => {
            println!("auth_native_dump: OK (stream created, file={f} pos={p})");
            drop(s);
        }
        Err(e) => println!("auth_native_dump: ERR {e:?}"),
    }
    // ssl-mode / tampered query param passthrough into Opts
    for q in ["?ssl-mode=PREFERRED", "&bogus_param=1"] {
        let url = if q.starts_with('?') && !url_native.contains('?') {
            format!("{url_native}{q}")
        } else {
            format!(
                "{url_native}{}{q}",
                if url_native.contains('?') { "" } else { "?" }
            )
        };
        match Opts::from_url(&url) {
            Ok(o) => println!(
                "opts_parse {q:?} ok ssl_opts_some={}",
                o.get_ssl_opts().is_some()
            ),
            Err(e) => println!("opts_parse {q:?} err {e:?}"),
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let some_or = |i: usize| args.get(i).map(String::as_str);
    match args.get(1).map(String::as_str) {
        Some("dump") => {
            let url = args
                .get(2)
                .expect("usage: dump <url> <secs> [hb] [file pos]")
                .to_string();
            let secs: u64 = some_or(3).and_then(|s| s.parse().ok()).unwrap_or(10);
            let hb: u64 = some_or(4).and_then(|s| s.parse().ok()).unwrap_or(0);
            cmd_dump(
                &url,
                secs,
                hb,
                some_or(5),
                some_or(6).and_then(|s| s.parse().ok()),
            );
        }
        Some("auth") => {
            let a = args
                .get(2)
                .expect("usage: auth <sha2_url> <native_url>")
                .to_string();
            let b = args
                .get(3)
                .expect("usage: auth <sha2_url> <native_url>")
                .to_string();
            cmd_auth(&a, &b);
        }
        _ => {
            println!("repl_spike: unknown args — nothing to do");
        }
    }
}
