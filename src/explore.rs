//! Enumerador exhaustivo y acotado de ejecuciones y estados de crash.
//!
//! Modelo (ver README, sección "Modelo"):
//! - Cada SQE se ejecuta de forma atómica en algún punto después de su
//!   envío, respetando LINK/HARDLINK (el sucesor espera al anterior),
//!   DRAIN (espera a todo lo enviado antes y bloquea lo enviado después)
//!   y las esperas de CQE del programa.
//! - Un fsync exitoso vuelve durables las escrituras del mismo inodo que
//!   ya se ejecutaron. Un fsync con -EIO deja esas escrituras en estado
//!   incierto para siempre (las páginas quedan limpias, como en Linux):
//!   un fsync posterior no las cubre.
//! - Al crash, cada escritura no durable puede haber llegado entera, no
//!   haber llegado, o haber llegado por sectores (desgarro de 512 B).
//!   Cada rename no sincronizado con fsync_dir puede o no persistir.
//! - Si una SQE con LINK falla, el resto de la cadena completa con
//!   -ECANCELED. Con HARDLINK la cadena sigue.

use crate::model::*;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct Config {
    /// Inyecta -EIO como resultado posible de cada fsync y fsync_dir.
    pub fsync_eio: bool,
    /// Escrituras de hasta este número de sectores se desgarran con todos
    /// los subconjuntos; más grandes, solo con mitades.
    pub tear_all_subsets_up_to: usize,
    /// Tope de nodos del árbol de ejecución (0 = sin tope).
    pub max_nodes: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            fsync_eio: true,
            tear_all_subsets_up_to: 4,
            max_nodes: 2_000_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Counterexample {
    pub schedule: Vec<String>,
    pub crash_choice: Vec<String>,
    pub broken_record: String,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub name: String,
    pub nodes: u64,
    pub executions: u64,
    pub crash_points: u64,
    pub crash_states: u64,
    pub violations: u64,
    pub foreign_cancellations: u64,
    pub refused_acks: u64,
    pub deadlocks: u64,
    pub truncated: bool,
    pub elapsed_s: f64,
    pub first: Option<Counterexample>,
}

impl Report {
    pub fn found_bug(&self) -> bool {
        self.violations > 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Status {
    Pending,
    Done(i32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Dur {
    Volatile,
    Durable,
    Unsure,
}

#[derive(Clone)]
struct Inst {
    sqe: Rc<Sqe>,
    chain_prev: Option<usize>,
    status: Status,
}

#[derive(Clone)]
struct WriteEv {
    id: String,
    inode: usize,
    off: u64,
    data: Rc<Vec<u8>>,
    dur: Dur,
}

#[derive(Clone)]
struct RenameEv {
    id: String,
    from: String,
    to: String,
    durable: bool,
}

#[derive(Clone)]
struct St {
    pc: usize,
    sqes: Vec<Inst>,
    acked: Vec<String>,
    writes: Vec<WriteEv>,
    renames: Vec<RenameEv>,
    ns: BTreeMap<String, usize>,
    events: Vec<String>,
    foreign_cancel: bool,
}

struct Ctx<'a> {
    p: &'a Protocol,
    cfg: &'a Config,
    fd_inode: BTreeMap<String, usize>,
    init_ns: BTreeMap<String, usize>,
    rep: Report,
}

pub fn explore(p: &Protocol, cfg: &Config) -> Report {
    let mut fd_inode = BTreeMap::new();
    let mut init_ns = BTreeMap::new();
    for (i, (fd, name)) in p.files.iter().enumerate() {
        fd_inode.insert(fd.clone(), i);
        init_ns.insert(name.clone(), i);
    }
    let mut ctx = Ctx {
        p,
        cfg,
        fd_inode,
        init_ns: init_ns.clone(),
        rep: Report {
            name: p.name.clone(),
            ..Default::default()
        },
    };
    let st = St {
        pc: 0,
        sqes: vec![],
        acked: vec![],
        writes: vec![],
        renames: vec![],
        ns: init_ns,
        events: vec![],
        foreign_cancel: false,
    };
    let t0 = Instant::now();
    dfs(&mut ctx, st);
    ctx.rep.elapsed_s = t0.elapsed().as_secs_f64();
    ctx.rep
}

fn find(st: &St, id: &str) -> Option<usize> {
    st.sqes.iter().position(|i| i.sqe.id == id)
}

fn submit(st: &mut St, batch: &[Sqe]) {
    let base = st.sqes.len();
    for (k, s) in batch.iter().enumerate() {
        let chain_prev = if k > 0 {
            let prev = &batch[k - 1];
            if prev.has(Flag::Link) || prev.has(Flag::Hardlink) {
                Some(base + k - 1)
            } else {
                None
            }
        } else {
            None
        };
        st.sqes.push(Inst {
            sqe: Rc::new(s.clone()),
            chain_prev,
            status: Status::Pending,
        });
        st.events.push(format!("submit {}", s.id));
    }
}

/// Avanza el programa de la app hasta que se bloquea o termina.
fn advance(ctx: &mut Ctx, st: &mut St) {
    while st.pc < ctx.p.steps.len() {
        match &ctx.p.steps[st.pc] {
            Step::Submit(b) => submit(st, b),
            Step::Wait(ids) => {
                let blocked = ids
                    .iter()
                    .any(|id| find(st, id).is_some_and(|i| st.sqes[i].status == Status::Pending));
                if blocked {
                    return;
                }
                st.events.push(format!("wait {:?} ok", ids));
            }
            Step::Ack(a) => {
                let ok = a.requires_ok.iter().all(|id| match find(st, id) {
                    None => true,
                    Some(i) => matches!(st.sqes[i].status, Status::Done(r) if r >= 0),
                });
                if ok {
                    st.acked.extend(a.records.iter().cloned());
                    st.events.push(format!("ACK {:?}", a.records));
                } else {
                    ctx.rep.refused_acks += 1;
                    st.events.push(format!("ack rechazado {:?}", a.records));
                }
            }
            Step::SubmitIfFailed(c) => match find(st, &c.cond) {
                None => {}
                Some(i) => match st.sqes[i].status {
                    Status::Pending => return,
                    Status::Done(r) if r < 0 => submit(st, &c.sqes),
                    Status::Done(_) => {}
                },
            },
        }
        st.pc += 1;
    }
}

fn ready(st: &St, i: usize) -> bool {
    let inst = &st.sqes[i];
    if inst.status != Status::Pending {
        return false;
    }
    if let Some(p) = inst.chain_prev {
        if st.sqes[p].status == Status::Pending {
            return false;
        }
    }
    for j in 0..i {
        if st.sqes[j].sqe.has(Flag::Drain) && st.sqes[j].status == Status::Pending {
            return false;
        }
    }
    if inst.sqe.has(Flag::Drain) && (0..i).any(|j| st.sqes[j].status == Status::Pending) {
        return false;
    }
    true
}

fn outcomes(cfg: &Config, sqe: &Sqe) -> Vec<i32> {
    match &sqe.kind {
        OpKind::Read { .. } if sqe.may_fail => vec![0, EIO],
        OpKind::Fsync { .. } | OpKind::FsyncDir if cfg.fsync_eio && !sqe.no_fault => {
            vec![0, EIO]
        }
        _ => vec![0],
    }
}

fn overlaps(off: u64, len: u64, range: Option<(u64, u64)>) -> bool {
    match range {
        None => true,
        Some((ro, rl)) => off < ro + rl && ro < off + len,
    }
}

fn execute(ctx: &Ctx, st: &mut St, i: usize, mut res: i32) {
    let sqe = st.sqes[i].sqe.clone();
    match &sqe.kind {
        OpKind::Write { record } => {
            let spec = &ctx.p.records[record];
            st.writes.push(WriteEv {
                id: sqe.id.clone(),
                inode: ctx.fd_inode[&spec.fd],
                off: spec.off,
                data: Rc::new(encode_record(record)),
                dur: Dur::Volatile,
            });
        }
        OpKind::WriteRaw { fd, off, data } => {
            st.writes.push(WriteEv {
                id: sqe.id.clone(),
                inode: ctx.fd_inode[fd],
                off: *off,
                data: Rc::new(data.clone()),
                dur: Dur::Volatile,
            });
        }
        OpKind::Fsync { fd, range } => {
            let ino = ctx.fd_inode[fd];
            for w in st.writes.iter_mut() {
                if w.inode == ino
                    && w.dur == Dur::Volatile
                    && overlaps(w.off, w.data.len() as u64, *range)
                {
                    w.dur = if res >= 0 { Dur::Durable } else { Dur::Unsure };
                }
            }
        }
        OpKind::FsyncDir => {
            if res >= 0 {
                for r in st.renames.iter_mut() {
                    r.durable = true;
                }
            }
        }
        OpKind::Read { .. } => {}
        OpKind::Rename { from, to } => match st.ns.remove(from) {
            Some(ino) => {
                st.ns.insert(to.clone(), ino);
                st.renames.push(RenameEv {
                    id: sqe.id.clone(),
                    from: from.clone(),
                    to: to.clone(),
                    durable: false,
                });
            }
            None => res = -2,
        },
    }
    st.sqes[i].status = Status::Done(res);
    st.events.push(format!("cqe {} res={}", sqe.id, res));
    if res < 0 && sqe.has(Flag::Link) {
        let is_read = matches!(sqe.kind, OpKind::Read { .. });
        let mut cur = i;
        while let Some(next) = (0..st.sqes.len()).find(|&k| st.sqes[k].chain_prev == Some(cur)) {
            if st.sqes[next].status != Status::Pending {
                break;
            }
            st.sqes[next].status = Status::Done(ECANCELED);
            st.events.push(format!(
                "cqe {} res={} (cadena)",
                st.sqes[next].sqe.id, ECANCELED
            ));
            if is_read {
                st.foreign_cancel = true;
            }
            cur = next;
        }
    }
}

fn dfs(ctx: &mut Ctx, mut st: St) {
    if ctx.cfg.max_nodes > 0 && ctx.rep.nodes >= ctx.cfg.max_nodes {
        ctx.rep.truncated = true;
        return;
    }
    ctx.rep.nodes += 1;
    advance(ctx, &mut st);
    check_crash(ctx, &st);
    let ready_ids: Vec<usize> = (0..st.sqes.len()).filter(|&i| ready(&st, i)).collect();
    if ready_ids.is_empty() {
        ctx.rep.executions += 1;
        if st.pc < ctx.p.steps.len() {
            ctx.rep.deadlocks += 1;
        }
        if st.foreign_cancel {
            ctx.rep.foreign_cancellations += 1;
        }
        return;
    }
    for i in ready_ids {
        for res in outcomes(ctx.cfg, &st.sqes[i].sqe) {
            let mut s2 = st.clone();
            execute(ctx, &mut s2, i, res);
            dfs(ctx, s2);
        }
    }
}

fn sector_span(w: &WriteEv) -> (u64, u64) {
    let first = w.off / SECTOR as u64;
    let last = (w.off + w.data.len() as u64 - 1) / SECTOR as u64;
    (first, last - first + 1)
}

fn masks_for(cfg: &Config, w: &WriteEv) -> Vec<u64> {
    if w.dur == Dur::Durable {
        return vec![u64::MAX];
    }
    let (_, n) = sector_span(w);
    let n = n as usize;
    let full = if n >= 64 { u64::MAX } else { (1u64 << n) - 1 };
    if n <= cfg.tear_all_subsets_up_to {
        (0..=full).collect()
    } else {
        let half = n / 2;
        let lo = (1u64 << half) - 1;
        vec![0, full, lo, full & !lo]
    }
}

fn check_crash(ctx: &mut Ctx, st: &St) {
    ctx.rep.crash_points += 1;
    if st.acked.is_empty() {
        ctx.rep.crash_states += 1;
        return;
    }
    let wmasks: Vec<Vec<u64>> = st.writes.iter().map(|w| masks_for(ctx.cfg, w)).collect();
    let rmasks: Vec<Vec<bool>> = st
        .renames
        .iter()
        .map(|r| {
            if r.durable {
                vec![true]
            } else {
                vec![false, true]
            }
        })
        .collect();
    let dims: Vec<usize> = wmasks
        .iter()
        .map(|m| m.len())
        .chain(rmasks.iter().map(|m| m.len()))
        .collect();
    let mut idx = vec![0usize; dims.len()];
    loop {
        ctx.rep.crash_states += 1;
        let nw = wmasks.len();
        // Namespace al crash.
        let mut ns = ctx.init_ns.clone();
        for (k, r) in st.renames.iter().enumerate() {
            if rmasks[k][idx[nw + k]] {
                if let Some(ino) = ns.remove(&r.from) {
                    ns.insert(r.to.clone(), ino);
                }
            }
        }
        let mut broken = None;
        for rec in &st.acked {
            let spec = &ctx.p.records[rec];
            let bytes = match ns.get(&spec.check) {
                None => vec![0u8; RECORD_LEN],
                Some(&ino) => image(st, &wmasks, &idx, ino, spec.off, RECORD_LEN),
            };
            if !record_valid(&bytes, rec) {
                broken = Some(rec.clone());
                break;
            }
        }
        if let Some(rec) = broken {
            ctx.rep.violations += 1;
            if ctx.rep.first.is_none() {
                let mut choice = vec![];
                for (k, w) in st.writes.iter().enumerate() {
                    choice.push(format!(
                        "{} {:?} sectores_persistidos=0b{:b}",
                        w.id,
                        w.dur,
                        wmasks[k][idx[k]] & ((1u64 << sector_span(w).1.min(63)) - 1)
                    ));
                }
                for (k, r) in st.renames.iter().enumerate() {
                    choice.push(format!("{} persistido={}", r.id, rmasks[k][idx[nw + k]]));
                }
                ctx.rep.first = Some(Counterexample {
                    schedule: st.events.clone(),
                    crash_choice: choice,
                    broken_record: rec,
                });
            }
        }
        // Odómetro.
        let mut d = 0;
        loop {
            if d == dims.len() {
                return;
            }
            idx[d] += 1;
            if idx[d] < dims[d] {
                break;
            }
            idx[d] = 0;
            d += 1;
        }
    }
}

fn image(st: &St, wmasks: &[Vec<u64>], idx: &[usize], ino: usize, off: u64, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    for (k, w) in st.writes.iter().enumerate() {
        if w.inode != ino {
            continue;
        }
        let mask = wmasks[k][idx[k]];
        let (first, n) = sector_span(w);
        for s in 0..n {
            if mask & (1u64 << s.min(63)) == 0 {
                continue;
            }
            let sec_lo = ((first + s) * SECTOR as u64).max(w.off);
            let sec_hi = ((first + s + 1) * SECTOR as u64).min(w.off + w.data.len() as u64);
            let lo = sec_lo.max(off);
            let hi = sec_hi.min(off + len as u64);
            if lo < hi {
                let src = &w.data[(lo - w.off) as usize..(hi - w.off) as usize];
                buf[(lo - off) as usize..(hi - off) as usize].copy_from_slice(src);
            }
        }
    }
    buf
}
