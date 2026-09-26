# Pre-registro: réplica de SC-retention en ornith:9b con 10 repeticiones

Fijado el 2026-09-26 ANTES de lanzar el sweep. Es el paso (a) que dejó
declarado el cierre de `docs/hypothesis-2026-08-13-sc-retention.md`
("réplica de ornith con 10 reps para potenciar el test"), con
pre-registro propio y SIN iteración del tratamiento.

## Antecedente

Pasada 1 (2026-08-16, `docs/sweep-sc-retention-ornith-2026-08-16.json`,
40 pares): sc-route 5/40 vs control 0/40; los 5 pares discordantes TODOS a
favor de la palanca; McNemar exacto p = 0,0625; el brazo tratado consumió
~5 % menos input tokens. Direccional-positivo puro, no significativo por
sí solo. El veredicto de agosto fue "no adoptar aún" conforme al riesgo
pre-registrado "n chico", sin inflar la suite a posteriori.

## Pregunta

¿La señal de ornith es real? Con 80 pares nuevos (10 reps × 8 tareas), un
efecto del tamaño observado (12,5 % vs 0 %) se separa del ruido.

## Hipótesis

- **H1**: la ruta durable (`sc-route`) aumenta el cumplimiento de la
  constraint post-compactación en ornith:9b.
- **H0**: no hay diferencia; los 5/0 de agosto fueron azar del sampling.
- **Predicción numérica si H1 y el efecto es el observado**: ~10/80 tratado
  vs ~0-1/80 control, McNemar p < 0,01, discordantes ≥ 80 % pro-palanca.
- **Predicción si H0**: discordantes repartidos (≤ 60 % pro-palanca) o
  ninguno.

## Instrumento: IDÉNTICO al de agosto, salvo las semillas

- Suite `crates/braze-bench/suites/sc-compaction.toml` (8 ítems; se
  verifica que el `suite_fingerprint` del JSON nuevo coincida con
  `500646b2f85c5b3b`).
- Brazos, pareados por (tarea, repetición):
  - tratado: `ollama:ornith:9b+ablate:tactical-window=8;tactical-threshold=10`
  - control: `ollama:ornith:9b+ablate:tactical-window=8;tactical-threshold=10;no-sc-route`
- `ornith:9b` digest `a75697c14589…` (el mismo de agosto; el bench lo
  registra en `ollama_model_digests`), Ollama 0.32.1 en Nitro.
- `--repetitions 10 --seed 100` → semillas 100-109 por repetición,
  **disjuntas** de las 42-46 de agosto (sorteos nuevos, no copias).
  Temperatura 0,2 (default), `--keep-alive 2m`, `--task-timeout-secs 600`,
  sin `--no-ollama-stop` (un solo modelo, pero la regla del 10-08 no
  cuesta nada aquí).
- `max_tokens` default. Las truncaciones "final response truncated by the
  token budget" (agosto: 7 control / 4 tratado) tienen causa candidata en
  el reasoning sin presupuesto de Ornith-1 (`docs/nota-ornith-1-repo-
  2026-08-17.md`). NO se toca: cambiarlo rompería la comparabilidad con
  las 5 reps de agosto (análisis secundario) y afecta a ambos brazos por
  igual, que el pareo absorbe. Si en cualquiera de los dos brazos superan
  el 25 % de las corridas, se declara degradación de instrumento y se
  reporta como tal.
- Harness: el commit actual de braze (el bench lo registra en
  `braze_git_commit`); difiere del de agosto (47414f4). Por eso el
  análisis primario es sobre las 10 reps nuevas SOLAS.

## Análisis, fijado

1. **Primario**: McNemar exacto sobre los 80 pares nuevos (el reporte del
   bench lo emite contra el primer brazo), más la dirección de los
   discordantes y el ratio de `input_tokens` tratado/control.
2. **Secundario, declarado**: pool de 15 reps (5 de agosto + 10 nuevas,
   120 pares) con el mismo test; se reporta con el caveat de heterogeneidad
   de harness entre pasadas.
3. Sin tests adicionales, sin subgrupos post hoc (el desglose por tarea se
   muestra, no se testea).

## Criterios de decisión, pre-registrados

- **Adoptar condicional (solo ornith, solo con constraints declarados)**
  si el primario da p < 0,05, los discordantes son ≥ 80 % pro-palanca y el
  brazo tratado NO consume más input tokens que el control (el costo de
  contexto de la predicción diferencial de agosto sigue sin aparecer).
- **Rechazar y publicar el matiz** si p ≥ 0,05 y los discordantes son
  ≤ 60 % pro-palanca: los 5/0 de agosto fueron azar.
- **Reportar como no concluyente** en cualquier otra combinación (p.ej.
  p < 0,05 pero con costo de contexto, o direccional sin significancia):
  se publica tal cual y NO se agregan repeticiones.
- gpt-oss sigue "no medible por floor" en esta suite; no entra aquí.

## Ejecución (comando exacto)

```
cd ~/proyectos/braze && BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434 RUST_LOG=braze_engine=info \
  target/release/braze-bench crates/braze-bench/suites/sc-compaction.toml \
  --backends "ollama:ornith:9b+ablate:tactical-window=8;tactical-threshold=10,ollama:ornith:9b+ablate:tactical-window=8;tactical-threshold=10;no-sc-route" \
  --repetitions 10 --seed 100 --keep-alive 2m --task-timeout-secs 600 \
  --output docs/sweep-sc-retention-ornith-r2-2026-09-26.json \
  > docs/sweep-sc-retention-ornith-r2-2026-09-26.log 2>&1
```

Duración esperada: agosto promedió 186 s por corrida (máx. 577 s, cerca
del timeout); 160 corridas ≈ 8 h en Nitro (RTX 3050 6 GB + CPU). Nitro
sin otra carga (`ollama ps` vacío al lanzar).
