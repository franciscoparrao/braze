//! Checkpoint incremental del sweep y watchdog de progreso (2026-10-20).
//!
//! Motivo: el sweep discriminante de Bonsai-2-9B del 2026-10-17 quedó
//! colgado ~2,8 días en una sola tarea (`mover_almacen_completo`) después de
//! que el engine abortara una tool en background, y el reinicio del nodo se
//! llevó las 96 filas ya corridas porque el JSON se escribía recién al final.
//! Tres defensas, de la más fina a la más gruesa:
//!
//! 1. Cada fila terminada se agrega como una línea JSON a
//!    `<output>.partial.jsonl` y se fuerza a disco. Con `--resume` el sweep
//!    retoma: las filas ya presentes no se vuelven a correr.
//! 2. `runner::run_isolated` corre cada fila en su propia tarea de tokio con
//!    un backstop externo, así que una corrida que bloquee su hilo no frena
//!    al resto del sweep (ver ahí).
//! 3. Un hilo del sistema operativo mata el proceso si no se registra
//!    ninguna fila durante `stall_limit` (código de salida 4): cubre el caso
//!    en que el runtime entero quede bloqueado. Con el checkpoint, la cola
//!    puede relanzar el mismo comando con `--resume`.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::BenchError;
use crate::metrics::TaskResult;

/// Clave de una fila: (backend tal como lo muestra el reporte, tarea, repetición).
pub type RowKey = (String, String, u32);

/// Código de salida del watchdog: distinto de 0/1 para que una cola lo
/// distinga de un sweep terminado o de un error de arranque.
pub const WATCHDOG_EXIT_CODE: i32 = 4;

static LAST_PROGRESS_SECS: AtomicU64 = AtomicU64::new(0);

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Marca progreso (una fila registrada) para el watchdog.
pub fn touch_progress() {
    LAST_PROGRESS_SECS.store(now_secs(), Ordering::Relaxed);
}

/// Lanza el watchdog: si pasan `stall_limit` sin `touch_progress`, imprime
/// el diagnóstico y termina el proceso. Las filas ya escritas al checkpoint
/// quedan en disco.
pub fn spawn_watchdog(stall_limit: Duration) {
    touch_progress();
    let limit = stall_limit.as_secs().max(1);
    std::thread::Builder::new()
        .name("braze-bench-watchdog".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs((limit / 20).clamp(1, 60)));
            let idle = now_secs().saturating_sub(LAST_PROGRESS_SECS.load(Ordering::Relaxed));
            if idle >= limit {
                eprintln!(
                    "\nbraze-bench: WATCHDOG — ninguna fila terminada en {idle}s (límite {limit}s). \
                     El proceso parece bloqueado; se termina con código {WATCHDOG_EXIT_CODE}. \
                     Las filas ya corridas están en el checkpoint: relanzar el mismo comando \
                     con --resume."
                );
                std::process::exit(WATCHDOG_EXIT_CODE);
            }
        })
        .expect("no se pudo lanzar el hilo del watchdog");
}

/// Ruta del checkpoint para un `--output` dado: `x.json` → `x.json.partial.jsonl`.
pub fn checkpoint_path(output: &Path) -> PathBuf {
    let mut name = output.as_os_str().to_owned();
    name.push(".partial.jsonl");
    PathBuf::from(name)
}

/// Lee las filas de un checkpoint. Una última línea truncada (el proceso
/// murió escribiéndola) se descarta con aviso; cualquier otra línea
/// ilegible es un error, porque significa que el archivo no es lo que dice.
pub fn load_rows(path: &Path) -> Result<Vec<TaskResult>, BenchError> {
    let file = File::open(path)?;
    let lines: Vec<String> = BufReader::new(file).lines().collect::<Result<_, _>>()?;
    let mut rows = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<TaskResult>(line) {
            Ok(row) => rows.push(row),
            Err(err) if i + 1 == lines.len() => {
                eprintln!(
                    "braze-bench: la última línea del checkpoint {} está incompleta ({err}); se descarta",
                    path.display()
                );
            }
            Err(err) => {
                return Err(BenchError::Startup(format!(
                    "checkpoint {} ilegible en la línea {}: {err}",
                    path.display(),
                    i + 1
                )));
            }
        }
    }
    Ok(rows)
}

/// Estado del checkpoint de un sweep.
pub struct Checkpoint {
    path: Option<PathBuf>,
    /// Filas cargadas con `--resume`, pendientes de re-emitir en su lugar
    /// del bucle (así el resto de la contabilidad —monitor secuencial,
    /// fail-fast, baseline del pareo— las ve igual que si se hubieran
    /// corrido ahora).
    pub resumed: HashMap<RowKey, TaskResult>,
}

impl Checkpoint {
    /// Sin `--output` no hay checkpoint (el sweep no persiste nada).
    /// Con `--resume` carga el checkpoint existente; sin él, un checkpoint
    /// previo se renombra a `.prev` en vez de borrarse.
    pub fn open(output: Option<&Path>, resume: bool) -> Result<Self, BenchError> {
        let Some(output) = output else {
            return Ok(Self { path: None, resumed: HashMap::new() });
        };
        let path = checkpoint_path(output);
        let mut resumed = HashMap::new();
        if path.exists() {
            if resume {
                for row in load_rows(&path)? {
                    resumed.insert((row.backend.clone(), row.task_id.clone(), row.repetition), row);
                }
                println!(
                    "braze-bench: --resume — {} fila(s) cargadas de {}",
                    resumed.len(),
                    path.display()
                );
                // Se reescribe limpio: las filas reanudadas se vuelven a
                // registrar en orden al re-emitirse en el bucle.
                std::fs::remove_file(&path)?;
            } else {
                let mut prev = path.as_os_str().to_owned();
                prev.push(".prev");
                std::fs::rename(&path, &prev)?;
                eprintln!(
                    "braze-bench: había un checkpoint previo; se conserva como {} (usar --resume para retomarlo)",
                    PathBuf::from(prev).display()
                );
            }
        } else if resume {
            eprintln!(
                "braze-bench: --resume sin checkpoint en {}; el sweep empieza de cero",
                path.display()
            );
        }
        Ok(Self { path: Some(path), resumed })
    }

    /// Agrega la fila recién registrada al checkpoint y la fuerza a disco.
    /// Un fallo de escritura avisa y no detiene el sweep: el checkpoint es
    /// una red de seguridad, no parte de la medición.
    pub fn record(&mut self, row: Option<&TaskResult>) {
        touch_progress();
        let (Some(path), Some(row)) = (&self.path, row) else {
            return;
        };
        let write = || -> std::io::Result<()> {
            let mut file = OpenOptions::new().create(true).append(true).open(path)?;
            let line = serde_json::to_string(row).map_err(std::io::Error::other)?;
            writeln!(file, "{line}")?;
            file.sync_data()
        };
        if let Err(err) = write() {
            eprintln!(
                "braze-bench: no se pudo escribir el checkpoint {}: {err}",
                path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "braze-bench-checkpoint-test-{tag}-{}-{}",
                std::process::id(),
                now_secs()
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn row(task: &str, rep: u32, passed: bool) -> TaskResult {
        let t: crate::task::TaskDef = toml::from_str(&format!(
            "id = \"{task}\"\nskill = \"single_tool\"\nprompt = \"p\"\n"
        ))
        .expect("task de prueba");
        let err = BenchError::Startup("x".into());
        let mut r = crate::metrics::harness_error_result("ollama:x", &t, rep, &err);
        r.passed = passed;
        r
    }

    #[test]
    fn checkpoint_path_appends_suffix() {
        assert_eq!(
            checkpoint_path(Path::new("/tmp/a/sweep.json")),
            PathBuf::from("/tmp/a/sweep.json.partial.jsonl")
        );
    }

    #[test]
    fn rows_round_trip_and_resume_loads_them() {
        let dir = TmpDir::new("roundtrip");
        let out = dir.path().join("s.json");
        let mut cp = Checkpoint::open(Some(&out), false).unwrap();
        cp.record(Some(&row("t1", 0, true)));
        cp.record(Some(&row("t1", 1, false)));
        let resumed = Checkpoint::open(Some(&out), true).unwrap().resumed;
        assert_eq!(resumed.len(), 2);
        assert!(resumed[&("ollama:x".into(), "t1".into(), 0)].passed);
        assert!(!resumed[&("ollama:x".into(), "t1".into(), 1)].passed);
    }

    #[test]
    fn truncated_last_line_is_dropped_but_garbage_in_the_middle_errors() {
        let dir = TmpDir::new("trunc");
        let path = dir.path().join("c.jsonl");
        let good = serde_json::to_string(&row("t1", 0, true)).unwrap();
        std::fs::write(&path, format!("{good}\n{{\"backend\":\"ollama")).unwrap();
        assert_eq!(load_rows(&path).unwrap().len(), 1);
        std::fs::write(&path, format!("basura\n{good}\n")).unwrap();
        assert!(load_rows(&path).is_err());
    }

    #[test]
    fn opening_without_resume_keeps_the_previous_checkpoint_aside() {
        let dir = TmpDir::new("prev");
        let out = dir.path().join("s.json");
        let mut cp = Checkpoint::open(Some(&out), false).unwrap();
        cp.record(Some(&row("t1", 0, true)));
        let fresh = Checkpoint::open(Some(&out), false).unwrap();
        assert!(fresh.resumed.is_empty());
        assert!(dir.path().join("s.json.partial.jsonl.prev").exists());
    }
}
