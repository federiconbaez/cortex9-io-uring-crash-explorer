//! Pruebas de la semántica del modelo: LINK, HARDLINK, DRAIN, rango de
//! fsync y escrituras desgarradas.

use uring_crash::model::*;
use uring_crash::{explore, Config};

fn proto(json: &str) -> Protocol {
    serde_json::from_str(json).unwrap()
}

fn cfg() -> Config {
    Config {
        fsync_eio: false,
        ..Default::default()
    }
}

#[test]
fn ack_without_fsync_exposes_torn_record() {
    let p = proto(
        r#"{"name":"t","files":{"db":"db"},"records":{"r1":{"fd":"db","off":0,"check":"db"}},
        "steps":[{"submit":[{"id":"w1","op":"write","record":"r1"}]},{"wait":["w1"]},
        {"ack":{"records":["r1"],"requires_ok":["w1"]}}]}"#,
    );
    let r = explore(&p, &cfg());
    // 2 sectores -> 4 estados (nada, solo s0, solo s1, ambos); 3 rompen el record.
    assert_eq!(r.violations, 3);
    let c = r.first.unwrap();
    assert!(c.crash_choice[0].contains("Volatile"));
}

#[test]
fn torn_record_is_rejected_by_checksum() {
    let mut rec = encode_record("r1");
    assert!(record_checksum_ok(&rec));
    for b in rec[512..].iter_mut() {
        *b = 0;
    }
    assert!(!record_checksum_ok(&rec));
    assert!(!record_valid(&rec, "r1"));
}

#[test]
fn link_failure_cancels_rest_of_chain() {
    let p = proto(
        r#"{"name":"t","files":{"db":"db"},"records":{"r1":{"fd":"db","off":0,"check":"db"}},
        "steps":[{"submit":[{"id":"rd","op":"read","fd":"db","off":0,"len":512,"may_fail":true,"flags":["link"]},
        {"id":"w1","op":"write","record":"r1","flags":["link"]},{"id":"f1","op":"fsync","fd":"db"}]},
        {"wait":["f1"]},{"ack":{"records":["r1"],"requires_ok":["w1","f1"]}}]}"#,
    );
    let r = explore(&p, &cfg());
    assert_eq!(r.foreign_cancellations, 1);
    assert_eq!(r.refused_acks, 1, "la app que mira res rechaza el commit");
    assert_eq!(r.violations, 0);
}

#[test]
fn hardlink_failure_does_not_cancel() {
    let p = proto(
        r#"{"name":"t","files":{"db":"db"},"records":{"r1":{"fd":"db","off":0,"check":"db"}},
        "steps":[{"submit":[{"id":"rd","op":"read","fd":"db","off":0,"len":512,"may_fail":true,"flags":["hardlink"]},
        {"id":"w1","op":"write","record":"r1","flags":["link"]},{"id":"f1","op":"fsync","fd":"db"}]},
        {"wait":["f1"]},{"ack":{"records":["r1"],"requires_ok":["w1","f1"]}}]}"#,
    );
    let r = explore(&p, &cfg());
    assert_eq!(r.foreign_cancellations, 0);
    assert_eq!(r.refused_acks, 0);
    assert_eq!(r.violations, 0);
}

#[test]
fn drain_orders_fsync_after_all_writes() {
    let p = proto(
        r#"{"name":"t","files":{"db":"db"},"records":{"r1":{"fd":"db","off":0,"check":"db"},"r2":{"fd":"db","off":4096,"check":"db"}},
        "steps":[{"submit":[{"id":"w1","op":"write","record":"r1"},{"id":"w2","op":"write","record":"r2"},
        {"id":"f1","op":"fsync","fd":"db","flags":["drain"]}]},{"wait":["f1"]},
        {"ack":{"records":["r1","r2"],"requires_ok":["f1"]}}]}"#,
    );
    let r = explore(&p, &cfg());
    assert_eq!(
        r.executions, 2,
        "w1/w2 en cualquier orden, fsync siempre al final"
    );
    assert_eq!(r.violations, 0);
}

#[test]
fn ranged_fsync_that_misses_the_write_is_a_bug() {
    let p = proto(
        r#"{"name":"t","files":{"db":"db"},"records":{"r1":{"fd":"db","off":8192,"check":"db"}},
        "steps":[{"submit":[{"id":"w1","op":"write","record":"r1","flags":["link"]},
        {"id":"f1","op":"fsync","fd":"db","range":[0,4096]}]},{"wait":["f1"]},
        {"ack":{"records":["r1"],"requires_ok":["w1","f1"]}}]}"#,
    );
    assert!(explore(&p, &cfg()).found_bug());
}

#[test]
fn fsync_eio_then_successful_fsync_does_not_save_data() {
    // Idea #10: tras -EIO las páginas quedan limpias; el segundo fsync no
    // cubre la escritura vieja.
    let p = proto(
        r#"{"name":"t","files":{"db":"db"},"records":{"r1":{"fd":"db","off":0,"check":"db"}},
        "steps":[{"submit":[{"id":"w1","op":"write","record":"r1","flags":["link"]},{"id":"f1","op":"fsync","fd":"db"}]},
        {"wait":["f1"]},{"submit_if_failed":{"cond":"f1","sqes":[{"id":"f2","op":"fsync","fd":"db","no_fault":true}]}},
        {"wait":["f2"]},{"ack":{"records":["r1"],"requires_ok":["f2"]}}]}"#,
    );
    let r = explore(&p, &Config::default());
    assert!(r.found_bug());
    let c = r.first.unwrap();
    assert!(c.schedule.iter().any(|e| e == "cqe f1 res=-5"));
    assert!(c.crash_choice[0].contains("Unsure"));
}

#[test]
fn node_budget_marks_report_truncated() {
    let p = proto(
        r#"{"name":"t","files":{"db":"db"},"records":{},
        "steps":[{"submit":[{"id":"a","op":"fsync","fd":"db"},{"id":"b","op":"fsync","fd":"db"},{"id":"c","op":"fsync","fd":"db"}]}]}"#,
    );
    let r = explore(
        &p,
        &Config {
            max_nodes: 3,
            ..cfg()
        },
    );
    assert!(r.truncated);
}
