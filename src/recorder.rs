//! Grabador sobre io_uring real (crate `io-uring`): ejecuta las SQEs en
//! el kernel y deja una traza (`Protocol`) que el explorador puede
//! recorrer. No necesita root: solo `io_uring_setup`.

use crate::model::*;
use io_uring::{opcode, squeue, types, IoUring};
use std::collections::{BTreeMap, HashMap};
use std::os::fd::RawFd;

pub struct RecordingRing {
    ring: IoUring,
    name: String,
    files: BTreeMap<String, String>,
    fds: HashMap<String, RawFd>,
    records: BTreeMap<String, RecordSpec>,
    steps: Vec<Step>,
    batch: Vec<Sqe>,
    buffers: Vec<Vec<u8>>,
    next: u64,
    ids: HashMap<u64, String>,
    results: HashMap<String, i32>,
    in_flight: usize,
}

fn sq_flags(flags: &[Flag]) -> squeue::Flags {
    let mut f = squeue::Flags::empty();
    for fl in flags {
        f |= match fl {
            Flag::Link => squeue::Flags::IO_LINK,
            Flag::Hardlink => squeue::Flags::IO_HARDLINK,
            Flag::Drain => squeue::Flags::IO_DRAIN,
        };
    }
    f
}

impl RecordingRing {
    /// Falla (no se saltea) si el kernel no permite `io_uring_setup`.
    pub fn new(name: &str, entries: u32) -> std::io::Result<Self> {
        Ok(RecordingRing {
            ring: IoUring::new(entries)?,
            name: name.to_string(),
            files: BTreeMap::new(),
            fds: HashMap::new(),
            records: BTreeMap::new(),
            steps: vec![],
            batch: vec![],
            buffers: vec![],
            next: 1,
            ids: HashMap::new(),
            results: HashMap::new(),
            in_flight: 0,
        })
    }

    pub fn register_file(&mut self, label: &str, file_name: &str, fd: RawFd) {
        self.files.insert(label.into(), file_name.into());
        self.fds.insert(label.into(), fd);
    }

    fn push(&mut self, sqe: Sqe, entry: squeue::Entry) {
        let ud = self.next;
        self.next += 1;
        self.ids.insert(ud, sqe.id.clone());
        let entry = entry.user_data(ud).flags(sq_flags(&sqe.flags));
        // SAFETY: los buffers viven en self.buffers hasta que termina el ring.
        unsafe {
            self.ring
                .submission()
                .push(&entry)
                .expect("cola de envío llena");
        }
        self.batch.push(sqe);
        self.in_flight += 1;
    }

    /// Escribe el record `record` en (label, off). El nombre de chequeo es
    /// el archivo actual del label.
    pub fn write_record(&mut self, id: &str, label: &str, off: u64, record: &str, flags: &[Flag]) {
        let data = encode_record(record);
        self.records.insert(
            record.into(),
            RecordSpec {
                fd: label.into(),
                off,
                check: self.files[label].clone(),
            },
        );
        self.buffers.push(data);
        let buf = self.buffers.last().unwrap();
        let entry = opcode::Write::new(types::Fd(self.fds[label]), buf.as_ptr(), buf.len() as u32)
            .offset(off)
            .build();
        let sqe = Sqe {
            id: id.into(),
            kind: OpKind::Write {
                record: record.into(),
            },
            flags: flags.to_vec(),
            may_fail: false,
            no_fault: false,
        };
        self.push(sqe, entry);
    }

    pub fn fsync(&mut self, id: &str, label: &str, flags: &[Flag]) {
        let entry = opcode::Fsync::new(types::Fd(self.fds[label])).build();
        let sqe = Sqe {
            id: id.into(),
            kind: OpKind::Fsync {
                fd: label.into(),
                range: None,
            },
            flags: flags.to_vec(),
            may_fail: false,
            no_fault: false,
        };
        self.push(sqe, entry);
    }

    /// Lectura que en la exploración puede fallar con -EIO.
    pub fn read(&mut self, id: &str, label: &str, off: u64, len: u32, flags: &[Flag]) {
        self.buffers.push(vec![0u8; len as usize]);
        let buf = self.buffers.last_mut().unwrap();
        let entry = opcode::Read::new(types::Fd(self.fds[label]), buf.as_mut_ptr(), len)
            .offset(off)
            .build();
        let sqe = Sqe {
            id: id.into(),
            kind: OpKind::Read {
                fd: label.into(),
                off,
                len: len as u64,
            },
            flags: flags.to_vec(),
            may_fail: true,
            no_fault: false,
        };
        self.push(sqe, entry);
    }

    pub fn submit(&mut self) -> std::io::Result<usize> {
        let n = self.ring.submit()?;
        let b = std::mem::take(&mut self.batch);
        self.steps.push(Step::Submit(b));
        Ok(n)
    }

    /// Espera las CQEs de `ids` y devuelve sus resultados del kernel.
    pub fn wait(&mut self, ids: &[&str]) -> std::io::Result<Vec<i32>> {
        while ids.iter().any(|id| !self.results.contains_key(*id)) {
            if self.in_flight == 0 {
                return Err(std::io::Error::other("esperando una id que no se envió"));
            }
            self.ring.submit_and_wait(1)?;
            let cqes: Vec<(u64, i32)> = self
                .ring
                .completion()
                .map(|c| (c.user_data(), c.result()))
                .collect();
            for (ud, res) in cqes {
                let id = self.ids[&ud].clone();
                self.results.insert(id, res);
                self.in_flight -= 1;
            }
        }
        self.steps
            .push(Step::Wait(ids.iter().map(|s| s.to_string()).collect()));
        Ok(ids.iter().map(|id| self.results[*id]).collect())
    }

    pub fn ack(&mut self, records: &[&str], requires_ok: &[&str]) {
        self.steps.push(Step::Ack(Ack {
            records: records.iter().map(|s| s.to_string()).collect(),
            requires_ok: requires_ok.iter().map(|s| s.to_string()).collect(),
        }));
    }

    pub fn into_protocol(self) -> Protocol {
        Protocol {
            name: self.name,
            files: self.files,
            records: self.records,
            steps: self.steps,
        }
    }
}
