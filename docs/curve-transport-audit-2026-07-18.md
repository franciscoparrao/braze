# Auditoría de transporte del sweep de la curva (2026-07-18)

Forense sobre los logs y JSONs commiteados del sweep de escala
(`docs/sweep-curva-multiescala-2026-07-10.*`), motivada por el incidente
de red del ancla BFCL: si una ráfaga de "error sending request" pudo
contaminar dos sweeps hoy, había que verificar si contaminó los sweeps
que el paper ya cita. **Sí, una celda de forma material.**

Criterio de clasificación (idéntico al aplicado en
`docs/emse-r2-analysis-2026-07-17.md` § 4 para el planner-ab):
`failure_cause == model_backend_error` **y** (`wall_time_ms < 1s` — el
request nunca llegó — o `run_error` contiene "request to model backend
failed" / "stream failed"). Los empty-response genuinos no califican.

## Hallazgo 1 (material): la celda `1B +plan+lead` está contaminada

El log `partial-1b.log` registra **30 warnings** de "planner call failed;
proceeding without a plan", todos con error de transporte, todos en el
mismo brazo, en 30 sesiones distintas (una por run):

| Brazo (slice 1B) | Raw | Transport | Limpio |
|---|---|---|---|
| `llama3.2:1b` | 18/95 (18.9%) | 0 | 18.9% |
| `llama3.2:1b+lead` | 85/95 (89.5%) | 0 | 89.5% |
| `llama3.2:1b+plan` | 0/95 (0.0%) | 0 | 0.0% |
| **`llama3.2:1b+plan+lead`** | **58/95 (61.1%)** | **30** | **58/65 = 89.2% [79.4, 94.7]** |

Los 30 runs con el planner caído puntúan **0/30**: no son "corridas sin
plan que igual funcionaron", son runs muertos por transporte (el fallo
del planner es el primer síntoma; el executor tampoco alcanzó al
servidor). Los 65 runs con plan real puntúan **58/65 = 89.2%**.

**Consecuencia para el paper (§curve, "Composing lead and planner
inherits the gradient")**: la afirmación *"at 1B the composition (61%)
still trails the lead alone (89%) by −28pp — the damage persists even
with a capable model opening the turn"* es **un artefacto de
infraestructura**. Neto de transporte, la composición a 1B es 89.2% vs
89.5% del lead solo: **estadísticamente idéntica**. La lectura correcta
es la contraria a la publicada: el lead **rescata completamente** el
daño del planner a TODAS las escalas medidas, incluida la más chica —
no es un mecanismo de "capacidad finita de recuperación".

Afecta también: la línea `+planner+lead` de la Fig. 1 en el punto 1B
(dibujada en 61%, debe ir en ~89%) y el párrafo de discusión que lee
esa celda como daño persistente.

## Hallazgo 2 (inmaterial, se disclosa): 8 runs sueltos en el slice qwen

| Brazo | Raw | Transport |
|---|---|---|
| `qwen2.5:3b` | 65/95 (68.4%) | 4 |
| `qwen2.5:7b` | 76/95 (80.0%) | 1 |
| `qwen3.5-coder` | 93/95 (97.9%) | 2 |
| `qwen3.5-coder+lead` | 89/95 (93.7%) | 1 |
| (los otros 8 brazos) | — | 0 |

Máximo desplazamiento si se excluyeran: +2.9pp (3B baseline 68.4→71.4%),
+0.9pp (7B), +2.1pp (coder), +1.0pp (coder+lead) — todos dentro de sus
propios intervalos de Wilson y sin cambiar ninguna comparación ni
veredicto. Se disclosan; no se re-corre por esto solo. El resto del
slice qwen está limpio, incluidas las cuatro celdas `+plan` que
sostienen el resultado del colapso del planner.

## Hallazgo 3: la degeneración empty-response NO es agotamiento de presupuesto

Responde la hipótesis (a) del reviewer blind (Issue 6:
"reasoning-budget exhaustion produciendo content vacío") **con datos ya
commiteados, sin sweep nuevo**:

- El engine emite un WARN explícito al truncar
  (`model output was truncated by max_tokens this round`, se dispara con
  `stop_reason` ∈ {`length`, `max_tokens`}). El tracing estaba activo en
  ambos slices (805 y 77 líneas WARN respectivamente).
- **Slice 1B**: 3 truncaciones en todo el archivo (2 con
  `output_tokens=4096`, o sea el tope real; 1 con 22). Los 37
  empty-response del brazo `+plan` generaron 47–594 tokens contra un
  presupuesto de **4096** (`Config::max_tokens`, el que usa el bench).
- **Slice qwen**: **cero** truncaciones; los 35 empty-response del
  ceiling generaron 44–619 tokens contra los mismos 4096.

Ningún empty-response coincide con el perfil de truncación, y ninguno
disparó el WARN que la truncación dispara siempre. **El presupuesto de
generación queda descartado como mecanismo en ambos extremos de la
escala.** Lo que sigue abierto (y el paper debe seguir declarando) es la
sub-hipótesis de plantilla/serving: que el template de chat trate un
mensaje assistant final como *prefill* y el modelo emita EOS inmediato.
Eso NO se responde variando `num_predict` — requiere inspeccionar el
prompt renderizado, lo que redefine el experimento encolado (ver
`docs/empty-response-discriminant-design-2026-07-18.md`).

## Acciones derivadas

1. **Re-run limpio de `1B +plan+lead`** (95 runs) — encolar en Nitro
   junto al re-run de los brazos coder; precedente: el re-run del brazo
   3B task-list del planner-ab. Reportar el re-run y disclosar el
   intento contaminado.
2. Corregir §curve, el párrafo de composición, la Fig. 1 y §threats con
   el número limpio (o el del re-run cuando exista).
3. Disclosar los 8 runs del Hallazgo 2 en §threats con su magnitud.
4. Reemplazar el experimento `num_predict` por el probe de plantilla
   (Hallazgo 3).
5. **Lección de infraestructura**: el retry de transporte
   (`BRAZE_OLLAMA_TRANSPORT_RETRIES`, commit 4334ce4) debería estar
   activo en TODO sweep futuro contra Nitro; esta auditoría muestra que
   el modo de falla no es nuevo ni raro, solo era invisible.
