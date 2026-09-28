//! Hooks de sesión externos (perfil operador, 2026-09-28): comandos del
//! config global (`hooks.session_start`, `hooks.post_compact`) cuya salida
//! entra al system prompt como sección "Session context". Es el puente con
//! el andamiaje del autor fuera de braze (`context_manager.py resume`,
//! `MEMORY.md`): el mismo script que Claude Code dispara en `SessionStart`
//! corre acá sin cambios, porque recibe por stdin el mismo JSON
//! `{"cwd","source","session_id"}` (`source` = `startup` | `resume` |
//! `compact`).
//!
//! Postura: los comandos vienen del config global (fuera del workdir), no
//! del modelo; su stdout es DATO para el prompt — capeado en bytes, bajo
//! timeout, y un hook roto degrada a "sin contexto" con warning, nunca
//! bloquea el arranque. El engine no spawnea procesos (sigue audit-only):
//! este módulo escribe en el [`SessionContextSlot`] que el engine lee al
//! armar cada request. El refresco tras compactación va por un
//! [`EngineHook`] que observa `CompactionOccurred` y despacha la corrida a
//! una tarea aparte — `on_event` corre bajo el timeout de 250 ms de los
//! hooks del engine, y un script externo no cabe ahí.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use braze_config::HookCommand;
use braze_engine::{EngineHook, SessionContextSlot};
use braze_events::AgentEvent;
use braze_types::SessionId;
use tokio::io::AsyncWriteExt;

/// Lo que cada hook recibe por stdin (serializado a JSON).
pub struct HookInput<'a> {
    pub cwd: &'a Path,
    pub source: &'a str,
    pub session_id: SessionId,
}

/// Corre los hooks en orden y concatena sus salidas no vacías con una
/// línea en blanco entre medio. `None` si ninguno produjo texto.
pub async fn run_hooks(hooks: &[HookCommand], input: &HookInput<'_>) -> Option<String> {
    let mut sections = Vec::new();
    for hook in hooks {
        match run_one(hook, input).await {
            Ok(out) if !out.trim().is_empty() => sections.push(out.trim_end().to_string()),
            Ok(_) => tracing::debug!(command = ?hook.command, "session hook sin salida"),
            Err(err) => tracing::warn!(
                command = ?hook.command,
                source = input.source,
                error = %err,
                "session hook falló; se ignora"
            ),
        }
    }
    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}

async fn run_one(hook: &HookCommand, input: &HookInput<'_>) -> Result<String, String> {
    let Some((program, args)) = hook.command.split_first() else {
        return Err("comando vacío".to_string());
    };
    let payload = serde_json::json!({
        "cwd": input.cwd,
        "source": input.source,
        "session_id": input.session_id.to_string(),
    })
    .to_string();
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .current_dir(input.cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("spawn: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        // Un hook que no lee stdin cierra su lado y el write falla: no es
        // error del hook.
        let _ = stdin.write_all(payload.as_bytes()).await;
        drop(stdin);
    }
    // Al expirar el timeout el future se dropea y `kill_on_drop` mata al
    // hijo — un hook colgado no queda huérfano.
    let output = tokio::time::timeout(
        Duration::from_secs(hook.timeout_secs),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| format!("timeout tras {}s", hook.timeout_secs))?
    .map_err(|e| format!("wait: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "exit {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if text.len() > hook.max_bytes {
        let mut cut = hook.max_bytes;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str(&format!(
            "\n[session hook output truncated at {} bytes]",
            hook.max_bytes
        ));
    }
    Ok(text)
}

/// Hook del engine que, tras cada `CompactionOccurred`, re-corre los
/// `post_compact` con `source = "compact"` y reemplaza el slot — solo si
/// produjeron texto (un hook mudo no borra el contexto anterior). La
/// corrida va a una tarea aparte: `on_event` vuelve de inmediato.
pub struct SessionHooksRunner {
    hooks: Vec<HookCommand>,
    slot: SessionContextSlot,
    cwd: PathBuf,
    live_session: Arc<Mutex<SessionId>>,
}

impl SessionHooksRunner {
    pub fn new(
        hooks: Vec<HookCommand>,
        slot: SessionContextSlot,
        cwd: PathBuf,
        live_session: Arc<Mutex<SessionId>>,
    ) -> Self {
        Self {
            hooks,
            slot,
            cwd,
            live_session,
        }
    }
}

#[async_trait]
impl EngineHook for SessionHooksRunner {
    fn id(&self) -> &str {
        "session-hooks"
    }

    async fn on_event(&self, event: &AgentEvent) -> Result<(), String> {
        if !matches!(event, AgentEvent::CompactionOccurred { .. }) || self.hooks.is_empty() {
            return Ok(());
        }
        let hooks = self.hooks.clone();
        let slot = Arc::clone(&self.slot);
        let cwd = self.cwd.clone();
        let session_id = *self
            .live_session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        tokio::spawn(async move {
            let input = HookInput {
                cwd: &cwd,
                source: "compact",
                session_id,
            };
            if let Some(text) = run_hooks(&hooks, &input).await {
                let bytes = text.len();
                *slot.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(text);
                tracing::info!(bytes, "session context refrescado tras compactación");
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(cwd: &Path) -> HookInput<'_> {
        HookInput {
            cwd,
            source: "startup",
            session_id: SessionId::new(),
        }
    }

    fn hook(command: &[&str], timeout_secs: u64, max_bytes: usize) -> HookCommand {
        HookCommand {
            command: command.iter().map(|s| s.to_string()).collect(),
            timeout_secs,
            max_bytes,
        }
    }

    /// El hook recibe el JSON por stdin (contrato SessionStart de Claude
    /// Code) y su stdout vuelve; un hook mudo no aporta sección; uno que
    /// falla o se cuelga se ignora sin tumbar a los demás; el cap corta.
    #[tokio::test]
    async fn hooks_receive_stdin_json_and_their_stdout_is_collected_capped_and_fault_tolerant() {
        let cwd = std::env::temp_dir();
        let echo_source = hook(
            &[
                "python3",
                "-c",
                "import sys,json; d=json.load(sys.stdin); print('src=' + d['source'] + ' cwd=' + d['cwd'])",
            ],
            5,
            8192,
        );
        let silent = hook(&["true"], 5, 8192);
        let failing = hook(&["false"], 5, 8192);
        let hanging = hook(&["sleep", "30"], 1, 8192);
        let long = hook(&["python3", "-c", "print('x' * 100)"], 5, 20);
        let out = run_hooks(
            &[echo_source, silent, failing, hanging, long],
            &input(&cwd),
        )
        .await
        .expect("dos hooks producen texto");
        assert!(
            out.starts_with(&format!("src=startup cwd={}", cwd.display())),
            "{out}"
        );
        assert!(out.contains("\n\nxxxxxxxxxxxxxxxxxxxx\n[session hook output truncated at 20 bytes]"), "{out}");
        assert!(run_hooks(&[hook(&["true"], 5, 8192)], &input(&cwd)).await.is_none());
        assert!(run_hooks(&[], &input(&cwd)).await.is_none());
    }

    /// Tras `CompactionOccurred` el runner re-corre los hooks con
    /// `source = compact` y reemplaza el slot; cualquier otro evento no
    /// lo toca.
    #[tokio::test]
    async fn runner_refreshes_the_slot_only_after_compaction() {
        let slot: SessionContextSlot = Arc::new(std::sync::RwLock::new(Some("viejo".into())));
        let runner = SessionHooksRunner::new(
            vec![hook(
                &["python3", "-c", "import sys,json; print('ctx:' + json.load(sys.stdin)['source'])"],
                5,
                8192,
            )],
            Arc::clone(&slot),
            std::env::temp_dir(),
            Arc::new(Mutex::new(SessionId::new())),
        );
        runner
            .on_event(&AgentEvent::UserMessage { text: "hola".into() })
            .await
            .unwrap();
        assert_eq!(slot.read().unwrap().as_deref(), Some("viejo"));
        runner
            .on_event(&AgentEvent::CompactionOccurred {
                summary: "s".into(),
                dropped_tokens_estimate: 3,
            })
            .await
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if slot.read().unwrap().as_deref() == Some("ctx:compact") {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "el slot no se refrescó");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}
