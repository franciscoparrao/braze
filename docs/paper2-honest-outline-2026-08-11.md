# Outline honesto del Paper 2: la frontera de amortización de la memoria procedimental

Fecha: 2026-08-11
Estado: **esqueleto — scoping de la versión que los DATOS sostienen**, no
el claim triunfante del protocolo original
(`docs/paper2-memory-distillation-protocol-2026-07-16.md`). Reencuadra la
línea "memoria procedimental" del framework (marcada "medida, condición
identificada") en el paper que se puede escribir HOY con la evidencia
cerrada, sin forzar un resultado que el proyecto mismo refutó.

## El giro (por qué NO es el paper del protocolo)

El protocolo de julio proponía cinco contribuciones alrededor de un claim
triunfante: *amortizar la escalación cloud destilando la intervención del
tutor en un `LearnedPlaybook` reutilizable*. **Los datos de braze no lo
sostienen.** Dos mediciones independientes, ambas cerradas, apuntan al
mismo lugar:

1. **Piloto M1 (human-playbook vs none), 3 tareas B de
   `rust_compile_repair`, n=20 c/u**
   (`docs/sweep-memory-distillation-3taskB-synthesis-2026-07-17.md`, 140
   corridas): el playbook ahorra tokens netos en **1 de 3** tareas —
   justo la que el modelo ya tiene memorizada (saturada). En las dos
   frescas CUESTA (+1076, +1132 tokens netos) porque el ahorro de rondas
   (+0.15, +0.35) no paga el costo fijo de ~200-270 tokens/ronda que el
   playbook inyecta en cada turno.

2. **A/B de `enable_project_memory` (seeded/empty/baseline), n≈102**
   (`docs/hypothesis-2026-08-04-project-memory-ab.md`): `seeded − baseline`
   parecía significativo (+13 tareas, McNemar p=0.011, Holm 0.021), pero
   el brazo `empty` —prompt idéntico al baseline por construcción— subió
   casi lo mismo solo, y `seeded − empty = +4, p=0.541`. **El contenido de
   la memoria no explicó el efecto.** El control del mismo prompt salvó la
   conclusión de un falso positivo publicable. No se promovió.

El framework ya hizo el giro honesto: de "propuesta" a **"medida,
condición identificada"**. Este outline es ese giro convertido en paper.

## El claim que los datos SÍ sostienen

**La memoria procedimental para un agente local-first tiene una frontera
de amortización estrecha y cuantificable, y la técnica ingenua cae del
lado equivocado de ella.** Formalmente, un playbook amortiza solo si

```
ahorro_de_rondas × costo_por_ronda_base  >  costo_fijo_de_inyección × rondas
```

y las mediciones muestran que en tareas frescas el `ahorro_de_rondas` es
demasiado chico (~0.15-0.35 rondas) frente al costo fijo (~200-270
tokens/ronda × cada ronda del turno). La condición se cumple donde el
modelo YA sabe la tarea —donde la memoria es redundante— y falla donde la
tarea es fresca —donde haría falta—. Es una **anti-correlación entre
cuándo la memoria ayuda y cuándo se necesita.**

## Contribuciones (reencuadradas a lo medido)

1. **Formalizar la condición de amortización** de la memoria procedimental
   inyectada-por-prompt para agentes locales: el balance costo-fijo /
   ahorro-de-rondas, con la anti-correlación como predicción central.
2. **Dos nulos independientes que la confirman**, con la disciplina
   metodológica que los hace creíbles: el control del mismo-prompt (que
   convirtió un +13 aparentemente significativo en un +4 nulo) como
   instrumento reusable — *sin ese brazo, el A/B se promovía con un falso
   positivo*.
3. **El benchmark multi-sesión A→B→H** como aporte metodológico
   (existe y corrió), independiente de que el resultado sea negativo.
4. **La lección de diseño**: para que la memoria procedimental pague en un
   agente local, o el costo de inyección baja drásticamente (memoria
   fuera del prompt, recuperada bajo demanda como el AGENTS.md JIT), o el
   ahorro de rondas sube (tareas donde la intervención cloud desbloquea
   una clase entera, no un bug puntual). Ambas son future work con
   dirección medida, no especulación.

## Por qué un nulo es publicable acá (no una derrota)

"Destilá la escalación cloud y reusala" **suena obviamente bueno** — es
justo el tipo de técnica plausible que la comunidad adoptaría sin medir.
Que NO pague, y saber POR QUÉ (el costo fijo de inyección por-ronda), es
un resultado con dientes — igual que el edit-fence contra la evidencia de
aider, o el TTC que empeora en modelos débiles. El proyecto tiene una
disciplina de nulos (constrained decoding, stencil, H2 potenciado,
lead-summary, TTC, edit-fence); este es el nulo de mayor apuesta
conceptual, porque la técnica es la más "obvia". El control del
mismo-prompt es además una contribución metodológica transferible.

## Qué existe y qué NO (honestidad de alcance)

EXISTE: el crate `braze-memory` (LearnedPlaybook tipado + lifecycle), el
schema (`learned-playbook-v1.schema.json`), el benchmark A→B→H, las 140
corridas del piloto M1 cerrado, el A/B de project-memory con su control.

NO existe (y NO hace falta para el paper honesto): los variantes
`summary`/`procedural`/`lead-fallback` del protocolo original siguen sin
pilotear. Pero el human-playbook —el más fuerte a priori— ya salió
negativo, y todos los otros agregan MÁS costo de inyección, no menos: su
resultado esperado empeora la historia, no la cambia. Correrlos sería
gastar Nitro para confirmar lo predicho. Se anota como "predicción, no
medido" en vez de bloquear el paper.

## Relación con el Paper 1 y venue

- **Paper 1** mide routing/composición de capacidad en tareas AISLADAS
  (curva harness-vs-escala, positiva, falsable, en submission EMSE).
  **Paper 2** mide amortización de capacidad ENTRE sesiones — eje
  ortogonal, por eso es paper aparte. Su resultado negativo no debilita al
  Paper 1; lo complementa (el harness compensa la escala DENTRO de una
  tarea; la memoria NO compensa ENTRE tareas al costo actual).
- **Venue**: un resultado negativo/condicional riguroso encaja en venues
  que valoran replicabilidad y "negative results" — el mismo EMSE (que
  Paper 1 targetea) publica estudios empíricos con resultados nulos si el
  método es sólido; o un venue de sistemas de agentes. El
  `/paper-match` decidirá con el manuscrito, no ahora.

## Threats to validity

- **Una sola familia de tareas** (`rust_compile_repair`): la condición de
  amortización podría cambiar en tareas donde la intervención cloud
  desbloquea más rondas. Se anota como la frontera de generalización.
- **Un solo modelo** (gpt-oss:20b/Nitro): el costo-fijo/ahorro-de-rondas
  depende del modelo; un modelo más débil podría tener ahorro de rondas
  mayor (más margen que recuperar). Direccional, no medido.
- **Inyección por-prompt específicamente**: el nulo es de la memoria EN EL
  PROMPT. Una memoria recuperada bajo demanda (fuera del contexto base,
  como el AGENTS.md JIT) tiene otro perfil de costo — es exactamente la
  contribución #4 como future work.

## Próximo paso concreto

Si se decide escribir: este outline es la espina. El orden de escritura
que minimiza riesgo — igual que Paper 1 — es empezar por la sección de
resultados (las dos tablas de nulos ya existen y están cerradas), luego el
modelo de amortización (la fórmula), y recién después la intro/related
work. NO correr los variantes faltantes: la predicción es que empeoran la
historia, y el paper honesto no los necesita.
