//! Graba protocolos ejecutados en io_uring real (sin root) y los explora.
//! Si el kernel no permite io_uring_setup, el test FALLA (no se saltea).

use std::os::fd::AsRawFd;
use uring_crash::model::Flag;
use uring_crash::recorder::RecordingRing;
use uring_crash::{explore, Config};

fn not_root() {
    assert_ne!(
        unsafe { libc::geteuid() },
        0,
        "este test tiene que correr sin root"
    );
}

fn run(link: bool) -> uring_crash::Report {
    not_root();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let mut ring = RecordingRing::new(if link { "real-linked" } else { "real-unlinked" }, 8)
        .expect("io_uring_setup falló: este entorno no permite io_uring");
    ring.register_file("db", "db", f.as_raw_fd());
    let fl: &[Flag] = if link { &[Flag::Link] } else { &[] };
    ring.write_record("w1", "db", 0, "r1", fl);
    ring.fsync("f1", "db", &[]);
    ring.submit().unwrap();
    let res = ring.wait(&["w1", "f1"]).unwrap();
    assert_eq!(res[0], 1024, "write real de 1024 bytes");
    assert_eq!(res[1], 0, "fsync real");
    ring.ack(&["r1"], &["w1", "f1"]);
    let on_disk = std::fs::read(&path).unwrap();
    assert!(uring_crash::model::record_valid(&on_disk, "r1"));
    let p = ring.into_protocol();
    explore(
        &p,
        &Config {
            fsync_eio: true,
            ..Default::default()
        },
    )
}

#[test]
fn recorded_unlinked_trace_is_flagged() {
    assert!(run(false).found_bug());
}

#[test]
fn recorded_linked_trace_is_clean() {
    assert_eq!(run(true).violations, 0);
}
