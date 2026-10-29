//! Medición sobre el corpus pre-registrado: 6/6 bugs y 0/6 falsos positivos.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use uring_crash::{explore, load_corpus, Config};

fn read(p: &str) -> String {
    std::fs::read_to_string(format!("{}/{}", env!("CARGO_MANIFEST_DIR"), p)).unwrap()
}

#[test]
fn corpus_hash_matches_preregistration() {
    let reg = read("corpus/PREREGISTRO.sha256");
    for line in reg.lines() {
        let (hash, path) = line.split_once("  ").unwrap();
        let got = hex(&Sha256::digest(read(path).as_bytes()));
        assert_eq!(got, hash, "{path} cambió después del pre-registro");
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn run(cfg: &Config) -> (usize, usize, usize, usize) {
    let corpus = load_corpus(&read("corpus/protocols.json")).unwrap();
    let expected: BTreeMap<String, String> =
        serde_json::from_str(&read("corpus/expected.json")).unwrap();
    assert_eq!(corpus.protocols.len(), 12);
    let (mut tp, mut bugs, mut fp, mut oks) = (0, 0, 0, 0);
    for p in &corpus.protocols {
        let r = explore(p, cfg);
        assert!(!r.truncated, "{} truncado", p.name);
        assert_eq!(r.deadlocks, 0, "{} deadlock", p.name);
        match expected[&p.name].as_str() {
            "bug" => {
                bugs += 1;
                tp += r.found_bug() as usize;
            }
            "ok" => {
                oks += 1;
                fp += r.found_bug() as usize;
            }
            other => panic!("etiqueta desconocida {other}"),
        }
    }
    (tp, bugs, fp, oks)
}

#[test]
fn corpus_six_of_six_bugs_zero_false_positives() {
    let (tp, bugs, fp, oks) = run(&Config::default());
    assert_eq!((tp, bugs), (6, 6), "bugs detectados");
    assert_eq!((fp, oks), (0, 6), "falsos positivos");
}

#[test]
fn without_fsync_eio_the_two_eio_bugs_are_missed() {
    // Ablación: la inyección de -EIO en el CQE de FSYNC es la que encuentra
    // los dos bugs de manejo de errores de fsync.
    let cfg = Config {
        fsync_eio: false,
        ..Default::default()
    };
    let (tp, bugs, fp, _) = run(&cfg);
    assert_eq!((tp, bugs, fp), (4, 6, 0));
}
