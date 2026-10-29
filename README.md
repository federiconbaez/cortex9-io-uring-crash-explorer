# uring-crash

[Español](#español) · [English](#english)

## Español

Explorador **exhaustivo y acotado** de estados de crash para protocolos de
commit que escriben con io_uring. Toma una traza de SQEs (escrita a mano en
JSON o grabada de un `io_uring` real con `RecordingRing`), enumera todos los
órdenes de ejecución que la semántica de io_uring permite (LINK, HARDLINK,
DRAIN, esperas de CQE), corta la luz en cada punto, enumera qué pudo llegar
al disco y verifica que todo record que la aplicación confirmó siga intacto.

### Qué está medido

Corpus pre-registrado de 12 protocolos (6 correctos, 6 con bug), fijado en
el primer commit con sus sha256 en `corpus/PREREGISTRO.sha256` antes de que
existiera el explorador. Un test verifica que los hashes no cambiaron.

Estado del pre-registro: **pre-registrado localmente, verificable por el orden
de los commits, sin timestamp externo**. Nada fue público antes de medir y la
fecha de un commit local se puede editar, así que es más débil que un
pre-registro publicado. El pre-registro vale **por el orden de los commits**: el corpus (5f67d09)
es anterior al commit que agrega el explorador (c3da9f2). Se comprueba con:

```
git log --reverse --format='%h %ad %s' --date=iso-strict
git merge-base --is-ancestor 5f67d09 c3da9f2 && echo "corpus anterior al explorador"
```

Los sha256 del corpus se publicaron en la sala de coordinación recién
**después** de medir; no hubo publicación externa previa del hash.

```
cargo run --release --bin uring-crash -- corpus corpus/protocols.json corpus/expected.json
```

| Métrica | Resultado |
|---|---|
| Bugs sembrados detectados | **6/6** |
| Falsos positivos en protocolos correctos | **0/6** |
| Sin inyección de -EIO en fsync (`--no-fsync-eio`) | 4/6, 0/6 FP |
| Curva N=1..8 writes en vuelo (`cargo run --release --example curve`) | N=8: 80.640 ejecuciones, 190.241 estados en 0,49 s en la box |

La ablación muestra que los dos bugs de manejo de errores de fsync (ignorar
el -EIO y reintentar el fsync después del -EIO) solo aparecen con la
inyección de -EIO en el CQE de FSYNC, que es la idea #10 absorbida acá.

Turso #772 está reproducido como test (`tests/turso_772.rs`):

1. un lote de writes terminado en fsync sin LINK ni DRAIN pierde frames
   confirmados (el fsync completa antes que un write);
2. la cadena writes → LINK → fsync lo cierra;
3. un pread de otra transacción encadenado en medio que falla devuelve toda
   la cadena con -ECANCELED: el commit falla por una lectura ajena, y si la
   app no revisa `res` pierde datos.

Todo corre sin root ni `/dev/kvm`. `tests/recorder.rs` ejecuta los
protocolos en io_uring real sobre un archivo temporal, verifica que el
proceso no es root y falla (no se saltea) si `io_uring_setup` no está
permitido.

### Modelo y límites (leer antes de confiar)

- Cada SQE se ejecuta de forma atómica en un punto. No se modelan escrituras
  parcialmente aplicadas en memoria, solo desgarro en disco por sectores de
  512 B (todos los subconjuntos hasta 4 sectores; mitades para más).
- fsync exitoso: vuelve durables los writes del mismo inodo ya ejecutados
  (con rango, solo los que se solapan). fsync con -EIO: esos writes quedan
  inciertos para siempre; un fsync posterior no los cubre. Es el
  comportamiento que describe la literatura de fsync de Linux; **no lo validé
  contra el kernel** en este repo.
- rename: persiste solo con un `fsync_dir` exitoso posterior; si no, puede o
  no haber llegado. No se modela la semántica real de cada filesystem (ext4
  `auto_da_alloc`, btrfs, xfs).
- Writes no durables que se solapan se aplican en orden de ejecución; un
  disco real podría reordenarlos.
- El grabador cubre Write, Read y Fsync. No cubre RenameAt, SQPOLL, IOPOLL,
  buffers registrados ni binarios cerrados.
- La exploración es exhaustiva dentro del tope `max_nodes` (2 M por
  defecto); si se corta, el reporte lo marca `TRUNCADO` y el CLI sale con
  error.
- No hay comparación medida contra turmoil-io-uring ni contra CrashMonkey.

### Fuentes

- tursodatabase/turso#772, leído (texto y comentarios) para este repo.

## English

A **bounded, exhaustive** crash-state explorer for commit protocols that
write through io_uring. It takes an SQE trace (hand-written JSON or recorded
from a real `io_uring` via `RecordingRing`), enumerates every execution order
the io_uring semantics allow (LINK, HARDLINK, DRAIN, CQE waits), cuts power at
every point, enumerates what may have reached the disk, and checks that every
record the application acknowledged is intact.

### What is measured

A pre-registered corpus of 12 protocols (6 correct, 6 buggy), committed in
the first commit with sha256 hashes in `corpus/PREREGISTRO.sha256` before the
explorer existed. A test checks the hashes did not change.
Status: **pre-registered locally, verifiable by commit order, no external
timestamp**; local commit dates can be edited, so this is weaker than a
published pre-registration. It holds **by commit order only** (corpus 5f67d09 precedes
the explorer commit c3da9f2; verify with the `git log` and
`git merge-base --is-ancestor` commands above). The corpus sha256 hashes were
shared in the coordination room only **after** measuring.

| Metric | Result |
|---|---|
| Seeded bugs detected | **6/6** |
| False positives on correct protocols | **0/6** |
| Without fsync -EIO injection | 4/6, 0/6 FP |
| N=1..8 in-flight writes curve | N=8: 80,640 executions, 190,241 states in 0.49 s on the dev box |

The two fsync error-handling bugs (ignoring -EIO, retrying fsync after -EIO)
are only found with -EIO injected into the FSYNC CQE.

Turso #772 is reproduced as a test: unlinked fsync loses acknowledged frames,
the linked chain fixes it, and a failing pread linked into the commit chain
cancels it with -ECANCELED.

Runs without root or `/dev/kvm`; the real io_uring tests assert a non-root
euid and fail (never skip) when `io_uring_setup` is not allowed.

### Model and limits

Atomic SQE execution; 512 B sector tearing on disk; fsync -EIO leaves the
covered writes permanently uncertain (assumption, not validated against the
kernel here); renames persist only after a successful directory fsync; no
per-filesystem semantics; recorder covers Write/Read/Fsync only; no measured
comparison against turmoil-io-uring or CrashMonkey.

## Licencia / License

Apache-2.0.
