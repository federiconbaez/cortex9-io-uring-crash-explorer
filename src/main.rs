use std::process::ExitCode;
use uring_crash::{explore, load_corpus, Config};

fn usage() -> ExitCode {
    eprintln!("uso: uring-crash corpus <protocols.json> <expected.json> [--no-fsync-eio]");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 || args[1] != "corpus" {
        return usage();
    }
    let corpus = match load_corpus(&std::fs::read_to_string(&args[2]).expect("leer corpus")) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("corpus inválido: {e}");
            return ExitCode::from(2);
        }
    };
    let expected: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&std::fs::read_to_string(&args[3]).expect("leer expected"))
            .expect("expected inválido");
    let cfg = Config {
        fsync_eio: !args.iter().any(|a| a == "--no-fsync-eio"),
        ..Default::default()
    };
    let (mut tp, mut fp, mut tn, mut fneg, mut bugs, mut oks) = (0, 0, 0, 0, 0, 0);
    let mut total_states = 0u64;
    let mut total_s = 0f64;
    let mut bad = false;
    println!(
        "{:<28} {:>8} {:>10} {:>11} {:>10} {:>9} {:>6}",
        "protocolo", "esperado", "ejecuc.", "est.crash", "violac.", "cancel.", "veredicto"
    );
    for p in &corpus.protocols {
        let r = explore(p, &cfg);
        total_states += r.crash_states;
        total_s += r.elapsed_s;
        let exp = expected.get(&p.name).map(String::as_str).unwrap_or("?");
        let verdict = if r.found_bug() { "BUG" } else { "ok" };
        match (exp, r.found_bug()) {
            ("bug", true) => {
                tp += 1;
                bugs += 1
            }
            ("bug", false) => {
                fneg += 1;
                bugs += 1
            }
            ("ok", true) => {
                fp += 1;
                oks += 1
            }
            ("ok", false) => {
                tn += 1;
                oks += 1
            }
            _ => bad = true,
        }
        if r.truncated || r.deadlocks > 0 {
            bad = true;
        }
        println!(
            "{:<28} {:>8} {:>10} {:>11} {:>10} {:>9} {:>6}{}{}",
            p.name,
            exp,
            r.executions,
            r.crash_states,
            r.violations,
            r.foreign_cancellations,
            verdict,
            if r.truncated { " TRUNCADO" } else { "" },
            if r.deadlocks > 0 { " DEADLOCK" } else { "" }
        );
        if let Some(c) = &r.first {
            println!("    contraejemplo: record {} roto", c.broken_record);
            println!("    traza: {}", c.schedule.join(" | "));
            println!("    crash: {}", c.crash_choice.join(", "));
        }
    }
    println!();
    println!("bugs detectados: {tp}/{bugs}  falsos negativos: {fneg}");
    println!("falsos positivos: {fp}/{oks}  correctos limpios: {tn}/{oks}");
    println!(
        "estados de crash: {total_states} en {total_s:.3} s ({:.0} estados/s), fsync_eio={}",
        total_states as f64 / total_s.max(1e-9),
        cfg.fsync_eio
    );
    if bad || fneg > 0 || fp > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
