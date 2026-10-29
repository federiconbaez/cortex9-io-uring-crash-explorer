//! Protocolo de commit: archivos, records, pasos de la aplicación y SQEs.

use serde::Deserialize;
use std::collections::BTreeMap;

pub const SECTOR: usize = 512;
pub const RECORD_LEN: usize = 1024;
pub const EIO: i32 = -5;
pub const ECANCELED: i32 = -125;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Flag {
    Link,
    Hardlink,
    Drain,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum OpKind {
    /// Escribe el record `record` en (fd, off) del record.
    Write {
        record: String,
    },
    /// Escribe bytes arbitrarios (lo usa el grabador de io_uring real).
    WriteRaw {
        fd: String,
        off: u64,
        data: Vec<u8>,
    },
    /// fsync/fdatasync del inodo detrás de `fd`, opcionalmente con rango.
    Fsync {
        fd: String,
        #[serde(default)]
        range: Option<(u64, u64)>,
    },
    Read {
        fd: String,
        off: u64,
        len: u64,
    },
    Rename {
        from: String,
        to: String,
    },
    FsyncDir,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Sqe {
    pub id: String,
    #[serde(flatten)]
    pub kind: OpKind,
    #[serde(default)]
    pub flags: Vec<Flag>,
    /// La operación puede completar con -EIO (lecturas).
    #[serde(default)]
    pub may_fail: bool,
    /// Excluye este fsync de la inyección global de -EIO.
    #[serde(default)]
    pub no_fault: bool,
}

impl Sqe {
    pub fn has(&self, f: Flag) -> bool {
        self.flags.contains(&f)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Ack {
    pub records: Vec<String>,
    /// La app confirma solo si estas CQEs llegaron con res >= 0.
    /// Las ids que nunca se enviaron se ignoran.
    pub requires_ok: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CondSubmit {
    pub cond: String,
    pub sqes: Vec<Sqe>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Submit(Vec<Sqe>),
    Wait(Vec<String>),
    Ack(Ack),
    /// Envía `sqes` solo si la CQE de `cond` llegó con error.
    SubmitIfFailed(CondSubmit),
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecordSpec {
    pub fd: String,
    pub off: u64,
    /// Nombre de archivo donde la recuperación busca el record.
    pub check: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Protocol {
    pub name: String,
    /// fd lógico -> nombre de archivo inicial (existente y durable).
    pub files: BTreeMap<String, String>,
    #[serde(default)]
    pub records: BTreeMap<String, RecordSpec>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Corpus {
    pub protocols: Vec<Protocol>,
}

/// Contenido de un record: magic, largo, crc32 del payload y payload.
pub fn encode_record(name: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(RECORD_LEN - 12);
    let seed = name
        .bytes()
        .fold(7u8, |a, b| a.wrapping_mul(31).wrapping_add(b));
    for i in 0..(RECORD_LEN - 12) {
        payload.push(seed.wrapping_add(i as u8));
    }
    let mut out = Vec::with_capacity(RECORD_LEN);
    out.extend_from_slice(b"UCR1");
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

/// Recuperación por defecto: el record es válido si magic y crc cierran
/// y el contenido es exactamente el esperado.
pub fn record_valid(bytes: &[u8], name: &str) -> bool {
    bytes.len() >= RECORD_LEN && bytes[..RECORD_LEN] == encode_record(name)[..]
}

pub fn record_checksum_ok(bytes: &[u8]) -> bool {
    if bytes.len() < 12 || &bytes[..4] != b"UCR1" {
        return false;
    }
    let len = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let crc = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    bytes.len() >= 12 + len && crc32fast::hash(&bytes[12..12 + len]) == crc
}
