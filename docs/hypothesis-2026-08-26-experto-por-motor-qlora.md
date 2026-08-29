# Hipótesis: QLoRA de qwen2.5:3b sobre trayectorias del experto (experto-por-motor, paso 1)

Fecha: 2026-08-26
Estado: proposed — commiteado ANTES de exportar el JSONL, de destilar
nada y de tocar un peso (registro git-only; ver § Registro).
Línea: experto-por-motor. El paso cero (¿cuánto compra el harness solo,
antes de entrenar?) ya está medido en
`docs/sweep-pizzeria-pilot-v3-2026-08-14.json`. Esto es el paso 1.

## Origen

El piloto pizzeria del 14-ago dejó un gap grande sobre un motor que
ningún modelo conoce del pretraining:

| backend | passed |
|---|---|
| `ollama:gpt-oss:20b` (experto) | 16/18 |
| `ollama:ornith:9b` | 15/18 |
| `ollama:lfm2.5` | 12/18 |
| `ollama:qwen2.5:3b` (alumno) | **2/18** |

La pregunta de la línea es si las trayectorias del experto, destiladas
por SFT, mueven al alumno hacia arriba — un 3B que opere el motor sin
tener que pagar un 20B por consulta.

El disparador de escribir esto ANTES de entrenar es la lectura de Ren
& Sutherland (ICLR 2025, arXiv:2407.10490v4, en `docs/`). Su
descomposición por paso `Δ log π ≈ -η·A·K·G` predice tres efectos
colaterales del SFT que **no son visibles evaluando in-distribution**,
que es exactamente como este experimento se evaluaría por defecto (6
tareas de entrenamiento = 6 tareas de test). Sin los brazos de abajo,
el experimento produciría un número alto y no interpretable.

## Qué se sabe y qué no (antes de medir)

- El gap 16/18 vs 2/18 es **medido**, no supuesto, y el alumno está
  lejos del techo: hay espacio real para mover la aguja.
- El set de entrenamiento son **16 trayectorias** (las corridas
  `passed` del experto). Es diminuto. Ren & Sutherland corren 5.000
  ejemplos sobre Pythia-410M…2.8B y Qwen1.5-0.5B/1.8B — régimen SLM
  como el nuestro, pero **dos órdenes de magnitud más datos**. Su
  descomposición es analítica y no depende de `n`; las *tendencias*
  que verifican empíricamente podrían no transferir a `n=16`. Se
  tratan como hipótesis a medir con nuestro harness, no como
  resultados heredados.
- **El squeezing effect no aplica acá.** Es el resultado más citado
  del paper, pero requiere gradientes negativos (DPO y parientes).
  SFT/QLoRA no los tiene. Solo entraría si después se añadiera una
  fase de preferencias, y ahí la receta sería su método `extend`.
- Lo que **sí** aplica: (a) la presión *push-down* global sobre todo
  `y ≠ y⁺`, que degrada por construcción lo no representado en el
  set; (b) el aumento sostenido de confianza sobre `y⁺_{j≠u}` —la
  respuesta correcta de OTRA pregunta del set—, su mecanismo propuesto
  para un tipo específico de alucinación; (c) el "fingerprint": bajo
  el eNTK, todas las respuestas de un mismo modelo generador resultan
  mutuamente similares *sin importar su distancia semántica*.
- Sobre (c) no hay teoría cerrada — los propios autores lo dejan como
  problema abierto. Se registra como confound declarado, no como
  efecto esperado.
- **No se sabe** si la conversión adaptador→GGUF preserva el
  comportamiento de tool-calling del alumno. Es infraestructura sin
  precedente en este proyecto (ver § Riesgos).

## Pregunta

¿Destilar por SFT las 16 trayectorias del experto sobre `qwen2.5:3b`
mejora su pass rate operando el motor pizzeria — y a qué precio en
capacidades que el set de entrenamiento no cubre?

La segunda mitad de la pregunta no es un adorno: es la mitad que el
diseño por defecto no mediría.

## Diseño

| | |
|---|---|
| Alumno | `qwen2.5:3b` (baseline medido: 2/18) |
| Experto | `gpt-oss:20b` (16 trayectorias `passed` del piloto v3) |
| Tratamiento | QLoRA sobre el JSONL de `braze-bench export-sft`, filtro por defecto (`passed` funcional) |
| Brazo **A** | alumno base, sin tocar |
| Brazo **B** | alumno + adaptador QLoRA |
| Brazo **E** | **A/A**: alumno base repetido (piso de ruido in-sweep) |
| Seeds | 42, 43, 44 |

### Las tres suites, y por qué tres

| suite | rol | qué mide |
|---|---|---|
| `pizzeria-pilot.toml` (6 tareas) | **held-in** | ¿aprendió algo? Cota superior optimista: son las tareas del entrenamiento |
| `pizzeria-holdout.toml` (a construir, ver abajo) | **generalización** | ¿aprendió a operar el motor, o memorizó 6 trayectorias? |
| `discriminating.toml` (34 tareas) | **regresión** | la presión push-down: ¿qué se rompió que antes funcionaba? |

`discriminating.toml` es el brazo de **regresión**, no de
contaminación cruzada — corrige lo que asumí al proponer esto: el set
de entrenamiento es pizzeria, no discriminating, así que la
contaminación cruzada se mide *dentro* de la familia pizzeria, entre
sus tareas. Ambas cosas hacen falta y son distintas.

### La suite held-out — CONSTRUIDA Y CONGELADA (2026-08-29)

`crates/braze-bench/suites/pizzeria-holdout.toml`, **12 tareas**, el
máximo que el pre-registro autorizaba ("si el held-out puede
construirse con 12+ tareas sin inflar el costo, se hace — pero se decide
y se commitea AHORA, no después de ver A"). Commiteada antes de exportar
el JSONL y antes de entrenar.

Verificado al construirla:

- **Motor byte-idéntico** al del piloto (comparación exacta de los
  `setup_files`). Sin eso el held-out mediría "cambió la tarea" en vez
  de "generalizó".
- **Cero solape de ids** con el set de entrenamiento.
- **Ninguna respuesta esperada coincide con una entrenada**: el set
  enseña 11900 (napolitana familiar) y 19800 (napolitana familiar +
  margarita mediana), y ninguno aparece acá. Responder 11900 a una
  pregunta de precio de esta suite es, por construcción, aplicar el
  valor de una tarea entrenada a una held-out — el efecto de § 4.1 de
  Ren & Sutherland hecho observable sin instrumentación extra.
- Cobertura: 4 `single_tool`, 4 `multi_step`, 3 `error_recovery` con
  modos de fallo **distintos** al entrenado (que era pizza inexistente:
  acá son tamaño inválido, agregar tras confirmar, y comando
  desconocido), y 1 `distractor_selection` nuevo sobre un ítem no
  entrenado.

Dos aserciones de la primera versión se descartaron por vacuas: un
`expect_text_contains = "0"` y otro `= "1"` matchean casi cualquier
salida. Es la lección de la suite discriminante v1
(`borrar_bloque_deprecado` era vacua porque el crate ya compilaba).
Reemplazadas por una diferencia de precios (3000) y un total confirmado
(16800), ambos inequívocos.

### El diseño original de la suite, para trazabilidad

`pizzeria-holdout.toml`: tareas nuevas **sobre el mismo motor**
`pizzeria.py`, sin ninguna en el set de entrenamiento. Se construye y
se commitea ANTES de entrenar, y no se toca después. Cobertura mínima:
un precio de un ítem distinto del entrenado, un pedido con
composición distinta, una recuperación de error con un modo de fallo
distinto, y un distractor nuevo. La contaminación cruzada se lee acá:
las tareas comparten motor y formato pero exigen respuestas
distintas, así que responder con el valor o la ruta de una tarea
*entrenada* es observable.

## Hipótesis y priors honestos

- **H1 (held-in sube)**: B supera a A en `pizzeria-pilot`. *Prior:
  probable*, y deliberadamente poco informativo — con 16 ejemplos
  sobre 6 tareas, memorizar es un camino disponible y barato.
- **H2 (generaliza)**: B supera a A en `pizzeria-holdout`. *Prior:
  incierto*. Es la hipótesis que la línea necesita que sea cierta.
- **H3 (regresión)**: B **no** cae fuera del piso A/E en
  `discriminating.toml`. *Prior: riesgo real a la baja*. La presión
  push-down es estructural, no un accidente de hiperparámetros, y 16
  ejemplos de una familia estrecha son el peor caso para ella.
- **H4 (contaminación cruzada)**: la tasa de respuestas que aplican el
  valor o la ruta de una tarea entrenada a una tarea held-out no sube
  respecto de A. *Prior: incierto, con mecanismo nombrado* — es la
  predicción específica de §4.1 de Ren & Sutherland.

## Métricas

Primaria: pass rate dual (`passed` y `passed_strict`) por suite,
McNemar exacto pareado por (tarea, seed) para B−A, contrastado contra
el piso A/E de **esa misma suite** (los pisos no se importan entre
suites — lección del gate mal calibrado del Study 2 del Paper 2).

Secundarias: `tool_call_names` por corrida (el sustrato de H4),
rondas, tokens, `schema_validation_failures`, `rescued_tool_calls`,
wall time, y `[RouteMiss]` — que con un modelo destilado deja de ser
ruido de banco y pasa a ser señal: adherencia de ruta al experto.

**H4 se opera así**: para cada corrida held-out fallida, se clasifica
si la respuesta contiene un valor-objetivo o una secuencia de tool
calls que pertenece a una tarea *del set de entrenamiento*. La
clasificación se hace contra la lista de valores/rutas entrenados,
fijada al commitear este documento. Se reporta como tasa por brazo.

MDE declarado al medir el piso A/E, antes de leer B.

## Criterios de decisión, pre-registrados

1. **Piso primero.** La discordancia A/E define piso y MDE en cada
   suite. Ningún contraste B−A se interpreta por debajo del piso de
   su propia suite.
2. **Adoptar la destilación como línea viva** si H2 se cumple (mejora
   fuera del piso en held-out) **y** H3 no se viola (sin caída fuera
   del piso en `discriminating`). Solo esa conjunción justifica el
   paso siguiente (engordar el set, más motores).
3. **Rechazar como "memorización, no competencia"** si H1 se cumple
   pero H2 no. Es un resultado útil y publicable: mide lo que 16
   demostraciones compran de verdad, y ancla el costo real de la
   línea.
4. **Rechazar por regresión** si H3 se viola, *aunque H2 se cumpla*.
   Un alumno que opera pizzeria y perdió capacidad general no es el
   objetivo de la línea. El precio se documenta con la magnitud.
5. **Nulo limpio** (ni held-in sube): se reporta que 16 trayectorias
   no alcanzan en este régimen, con el `n` como límite declarado de
   antemano y no como excusa posterior.
6. **Sin iteración de tratamiento.** Un solo QLoRA, hiperparámetros
   fijados antes de ver resultados y anotados abajo. Si el resultado
   decepciona, la salida es un pre-registro NUEVO, no un barrido de
   hiperparámetros sobre el mismo test.
7. **Sin ampliar el held-out después de verlo.** La suite se congela
   al commitear.
8. **Gate anti-copias (L-9)**: si las corridas de un mismo brazo
   salen idénticas entre seeds, aplica la cláusula de instrumento
   (`BRAZE_LOCAL_TEMP>0` / semillas por tanda) antes de leer nada.
9. Fallos de infraestructura fuera del denominador; >10% invalida el
   sweep (repetir una vez, completo).

## Limitación estructural, declarada antes de medir

`pizzeria-pilot` tiene **6 tareas**: cada ítem pesa 16,7 pp. El propio
proyecto ya aprendió esta lección — la suite discriminante v2 tiene 34
tareas precisamente porque con 8 cada ítem pesaba 12,5 pp y el ruido
se comía cualquier efecto plausible (`docs/noise-floor-2026-07-26.md`).
Con 6 tareas × 3 seeds, este experimento **no puede** resolver efectos
finos, y el held-out heredará el problema salvo que se construya más
grande.

Consecuencia aceptada: el diseño está potenciado para detectar un
efecto **grande** (el salto de 2/18 hacia el rango del experto), que
es el único que justificaría la línea. Un efecto moderado saldrá
no-concluyente, y eso se reporta como tal en vez de perseguirse
ampliando la suite a posteriori. Si el held-out puede construirse con
12+ tareas sin inflar el costo, se hace — pero se decide y se commitea
AHORA, no después de ver A.

## Riesgos anotados

- **Conversión adaptador→GGUF sin precedente en el proyecto.** El
  camino QLoRA → merge → GGUF → LocalBackend (o Ollama) no está
  probado acá, y el hallazgo del 21-jul es advertencia directa: los
  blobs de Ollama para gpt-oss y Gemma NO son GGUF de llama.cpp. Un
  smoke de tool-calling del alumno convertido **sin adaptador** es
  precondición: si el alumno base convertido no reproduce su 2/18,
  el pipeline de conversión está contaminando y no hay experimento
  que leer. Se corre ese smoke ANTES del QLoRA.
- **Procedencia del motor.** Si algún brazo corre por LocalBackend,
  `engine_version` debe estar poblado en los JSON (capacidad agregada
  hoy). Un sweep del alumno destilado sin registrar con qué motor
  corrió no es comparable contra nada.
- **El fingerprint del experto es un confound, no un efecto medible
  acá.** Si B mejora, este diseño no puede separar "adquirió la
  competencia" de "adquirió el idiolecto de gpt-oss y el banco lo
  premia". El held-out lo mitiga (mismo motor, tareas nuevas) pero no
  lo resuelve. Se declara como amenaza a la validez de constructo, y
  la separación queda para un experimento propio con un segundo
  experto de idiolecto distinto (`ornith:9b`, 15/18, es el candidato
  natural).
- **`qwen2.5:3b` es thinking-adjacent en su familia**: presupuestar
  tokens y verificar que el content no sale vacío, como ya está
  anotado para `qwen3.5-coder`.
- El experto acertó 16/18: las 2 fallidas quedan fuera por el filtro
  `passed`. No se incluyen "para dar más datos" — eso sería entrenar
  sobre trayectorias que el oráculo rechazó.

## Hiperparámetros, fijados antes de ver resultados

A completar y commitear en el mismo commit que este documento, ANTES
de entrenar: rank, alpha, dropout, learning rate, épocas, longitud de
secuencia, y si se enmascara el prompt. Sin esto fijado, el criterio 6
(sin iteración) no es verificable por terceros.

> PENDIENTE (autor): completar esta tabla antes del primer run.

## Registro y su caveat

Este documento se commitea y **pushea al repositorio público antes de
exportar el JSONL y antes de entrenar**, de modo que el orden sea
verificable:
`git log --diff-filter=A --format='%ad' -- <este archivo>` debe ser
anterior a la fecha del JSONL y de los JSON de resultados. Es la
práctica que la auditoría del Paper 2 mostró que **no** se cumplió
para el piloto M1, donde registro y datos entraron en un mismo commit
posterior.

## Referencias

- Ren, Y. & Sutherland, D. J. (2025). *Learning Dynamics of LLM
  Finetuning*. ICLR 2025. arXiv:2407.10490v4. Copia en
  `docs/2407.10490v4.pdf`.
- Piloto paso cero: `docs/sweep-pizzeria-pilot-v3-2026-08-14.json`.
- Suite del set de entrenamiento:
  `crates/braze-bench/suites/pizzeria-pilot.toml`.
- Exportador: `crates/braze-bench/src/sft.rs`.
