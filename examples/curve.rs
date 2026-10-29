//! Curva de explosión combinatoria: N writes en vuelo sin orden entre sí y
//! un fsync con DRAIN, más un write extra sin cubrir después del ack.
//! Uso: cargo run --release --example curve [N_MAX]

use uring_crash::model::*;
use uring_crash::{explore, Config};

fn proto(n: usize) -> Protocol {
    let mut batch = vec![];
    let mut records = std::collections::BTreeMap::new();
    let mut names = vec![];
    for i in 0..n {
        let r = format!("r{i}");
        records.insert(
            r.clone(),
            RecordSpec {
                fd: "db".into(),
                off: i as u64 * 4096,
                check: "db".into(),
            },
        );
        batch.push(Sqe {
            id: format!("w{i}"),
            kind: OpKind::Write { record: r.clone() },
            flags: vec![],
            may_fail: false,
            no_fault: false,
        });
        names.push(r);
    }
    batch.push(Sqe {
        id: "sync".into(),
        kind: OpKind::Fsync {
            fd: "db".into(),
            range: None,
        },
        flags: vec![Flag::Drain],
        may_fail: false,
        no_fault: false,
    });
    let mut req: Vec<String> = (0..n).map(|i| format!("w{i}")).collect();
    req.push("sync".into());
    Protocol {
        name: format!("n{n}"),
        files: [("db".to_string(), "db".to_string())].into(),
        records,
        steps: vec![
            Step::Submit(batch),
            Step::Wait(vec!["sync".into()]),
            Step::Ack(Ack {
                records: names,
                requires_ok: req,
            }),
        ],
    }
}

fn main() {
    let nmax: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);
    println!("N,nodos,ejecuciones,estados_crash,segundos,estados_por_s,violaciones,truncado");
    for n in 1..=nmax {
        let r = explore(&proto(n), &Config::default());
        println!(
            "{},{},{},{},{:.3},{:.0},{},{}",
            n,
            r.nodes,
            r.executions,
            r.crash_states,
            r.elapsed_s,
            r.crash_states as f64 / r.elapsed_s.max(1e-9),
            r.violations,
            r.truncated
        );
    }
}
