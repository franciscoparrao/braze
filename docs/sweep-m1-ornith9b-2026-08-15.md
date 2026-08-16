# Síntesis: replicación M1 en ornith:9b — replica y PROFUNDIZA

Fecha: 2026-08-16 (sweep corrido 2026-08-15/16, 4 tandas)
Pre-registro: `docs/hypothesis-2026-08-15-m1-ornith9b-replication.md`
(commiteado en b2fb8d9 ANTES de lanzar)
Datos: `docs/sweep-m1-ornith9b-batch{0..3}-2026-08-15.json` (140 corridas,
7 brazos-tarea × 20, seeds 42–61 en 4 tandas de 5, `ollama:ornith:9b`
en Nitro, timeout 300 s, temp 0.2)
Estado: **CERRADO**.

## Gate de infraestructura (criterio 3): PASA

9/140 filas con timeout/backend-error (6.4% < 10%). Un solo
`model_backend_error` (en `loop_none`, cuenta como fila perdida de
infraestructura). Los 8 timeouts se concentran en brazos playbook (6)
vs none (2) — ver § censura.

## Paso 1 pre-registrado: clasificación saturada/fresca de ornith (solo brazos none)

| tarea (none) | pass | rondas |
|---|---|---|
| original (E0502 canónico) | 16/20 (80%) | 3.95 |
| loop (E0502 iteración) | 12/20 (60%) | 3.50 |
| move (E0382) | 10/20 (50%) | 4.40 |

Para ornith:9b, `original` es la más sabida y `loop`/`move` son
frescas — **la misma clasificación que gpt-oss:20b**, declarada aquí
antes de leer los brazos playbook.

## Paso 2: contrastes none vs playbook

| par | pass n→p (Fisher) | rondas n→p | ΔR [CI 95%] (Holm) | ΔT tok/ronda | net Δtokens [CI] |
|---|---|---|---|---|---|
| original | 16→11 (p=0.18) | 3.95→4.45 | **−0.50** [−1.17,+0.17] (0.14) | +398 | **+2709** [+999,+4419] |
| loop | 12→5 (p=0.05) | 3.50→5.05 | **−1.55** [−2.16,−0.94] (<0.001) | +582 | **+5794** [+4011,+7577] |
| move | 10→2 (p=0.01) | 4.40→6.15 | **−1.75** [−3.26,−0.24] (0.049) | +527 | **+6635** [+2031,+11239] |

Holdout (fuera de familia, con playbook): 18/20, 3.35 rondas — igual
que en gpt-oss, el playbook inaplicable no descarrila.

ΔR\* break-even por tarea: 1.01 / 1.68 / 1.77 rondas (ΔT de ornith es
~2× el de gpt-oss: 398–582 vs 243–268 tok/ronda — el mismo playbook
cuesta más caro en este executor, que además parte de T_base más alto).

## Veredicto según los criterios pre-registrados: REPLICA (criterio 1) — y profundiza

- **El orden de la anti-correlación se sostiene** bajo la clasificación
  propia de ornith: el daño es menor en la tarea sabida (ΔR −0.50,
  n.s., pass −5pp n.s.) y máximo en la más fresca (ΔR −1.75, pass
  50%→10%, Fisher 0.01). `net_token_delta` positivo en los 3 pares con
  CIs que excluyen cero. H1 se cumple en su letra.
- **El régimen es más severo que en gpt-oss**: allá el playbook no
  ahorraba rondas suficientes (ΔR +0.15/+0.35, fallo *económico*);
  acá ΔR es **negativo** en todas partes — el playbook alarga las
  trayectorias — y **degrada el éxito en las frescas** con
  significancia (fallo *conductual*). Para el executor más débil, el
  "reminder" se vuelve distracción.
- **Mata la expectativa direccional del Threats original** ("un modelo
  más débil tiene más margen de rondas que recuperar"): la dirección
  medida es la CONTRARIA. Coherente con el precedente del plan-en-prosa
  del Paper 1 (contexto extra daña a los chicos).

## Censura por timeout (dirección conservadora)

6 de los 8 timeouts caen en brazos playbook. Las filas con timeout
tienen rondas/tokens truncados a los 300 s, así que las medias de los
brazos playbook SUBESTIMAN rondas y tokens verdaderos: el ΔR real es
más negativo y el costo neto real más alto que lo reportado. El sesgo
va en contra de la conclusión, no a favor.

## Nota menor

`loop_none` tiene 1 `model_backend_error`; contarlo dentro o fuera del
denominador mueve ese brazo entre 12/20 y 12/19 — no cambia ninguna
conclusión.

## Implicancia para el Paper 2

Gana la subsección "Replication on a second executor" (Results), y el
threat "un solo modelo" se reformula: dos executors de arquitectura
distinta (MoE 20B/3.6B activos; denso 9B), misma familia de tareas. La
frase "the failure mode is economic, not behavioral" (5.1/6.1) queda
condicionada al executor: económico en el 20B, también conductual en
el 9B — el modo de falla EMPEORA al bajar la escala.
