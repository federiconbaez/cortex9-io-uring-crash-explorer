//! Reproducción del escenario de tursodatabase/turso#772:
//! 1) un lote de writes terminado en fsync sin LINK/DRAIN: el fsync puede
//!    completar antes que los writes y un crash pierde datos confirmados;
//! 2) el arreglo con cadenas (writes -> LINK -> fsync) lo cierra;
//! 3) pero si un pread de otra transacción queda encadenado y falla, toda
//!    la cadena vuelve con -ECANCELED.

use uring_crash::model::*;
use uring_crash::{explore, Config};

fn w(id: &str, rec: &str, link: bool) -> Sqe {
    Sqe {
        id: id.into(),
        kind: OpKind::Write { record: rec.into() },
        flags: if link { vec![Flag::Link] } else { vec![] },
        may_fail: false,
        no_fault: false,
    }
}

fn fsync(id: &str) -> Sqe {
    Sqe {
        id: id.into(),
        kind: OpKind::Fsync {
            fd: "wal".into(),
            range: None,
        },
        flags: vec![],
        may_fail: false,
        no_fault: false,
    }
}

fn pread(id: &str, link: bool) -> Sqe {
    Sqe {
        id: id.into(),
        kind: OpKind::Read {
            fd: "wal".into(),
            off: 1 << 20,
            len: 4096,
        },
        flags: if link { vec![Flag::Link] } else { vec![] },
        may_fail: true,
        no_fault: false,
    }
}

fn commit(batch: Vec<Sqe>) -> Protocol {
    let mut records = std::collections::BTreeMap::new();
    for (i, r) in ["f1", "f2", "f3"].iter().enumerate() {
        records.insert(
            r.to_string(),
            RecordSpec {
                fd: "wal".into(),
                off: i as u64 * 4096,
                check: "db-wal".into(),
            },
        );
    }
    Protocol {
        name: "turso-772".into(),
        files: [("wal".to_string(), "db-wal".to_string())].into(),
        records,
        steps: vec![
            Step::Submit(batch),
            Step::Wait(vec!["w1".into(), "w2".into(), "w3".into(), "sync".into()]),
            Step::Ack(Ack {
                records: vec!["f1".into(), "f2".into(), "f3".into()],
                requires_ok: vec!["w1".into(), "w2".into(), "w3".into(), "sync".into()],
            }),
        ],
    }
}

fn cfg() -> Config {
    Config {
        fsync_eio: false,
        ..Default::default()
    }
}

#[test]
fn turso_772_unlinked_fsync_loses_committed_frames() {
    let p = commit(vec![
        w("w1", "f1", false),
        w("w2", "f2", false),
        w("w3", "f3", false),
        fsync("sync"),
    ]);
    let r = explore(&p, &cfg());
    assert!(r.found_bug(), "el bug de #772 tiene que aparecer");
    let c = r.first.unwrap();
    let pos = |e: &str| c.schedule.iter().position(|x| x.starts_with(e));
    // En el contraejemplo, el fsync completa antes que algún write.
    let sync = pos("cqe sync").unwrap();
    assert!(["cqe w1", "cqe w2", "cqe w3"]
        .iter()
        .any(|e| pos(e).is_none_or(|p| p > sync)));
}

#[test]
fn turso_772_linked_chain_fix_is_clean() {
    let p = commit(vec![
        w("w1", "f1", true),
        w("w2", "f2", true),
        w("w3", "f3", true),
        fsync("sync"),
    ]);
    let r = explore(&p, &cfg());
    assert_eq!(r.violations, 0);
}

#[test]
fn turso_772_linked_pread_failure_cancels_commit_chain() {
    // pread de otra transacción queda en medio de la cadena del commit.
    let p = commit(vec![
        w("w1", "f1", true),
        pread("other_tx_read", true),
        w("w2", "f2", true),
        w("w3", "f3", true),
        fsync("sync"),
    ]);
    let r = explore(&p, &cfg());
    assert_eq!(
        r.foreign_cancellations, 1,
        "pread falla -> cadena -ECANCELED"
    );
    assert_eq!(r.refused_acks, 1, "el commit falla por una lectura ajena");
    assert_eq!(r.violations, 0, "la app que revisa res no pierde datos");
    // Si la app no revisa res (cuenta la CQE como hecha), pierde datos.
    let mut bad = p.clone();
    if let Step::Ack(a) = &mut bad.steps[2] {
        a.requires_ok.clear();
    }
    assert!(explore(&bad, &cfg()).found_bug());
}
