# Nota: repo Ornith-1 (github.com/ornith-ai/Ornith-1) — qué explica de nuestros propios datos

Fecha: 2026-08-17. Fuente: README del repo (DeepReinforce AI). Contexto:
`ornith:9b` es nuestro segundo modelo que satura `default.toml` (95/95
bajo métrica dual) y protagonista de la replicación M1 y del sweep
SC-retention de esta semana.

## Datos duros

- Familia **Ornith-1.0**: 9B-Dense, 31B-Dense, 35B-MoE, 397B-MoE;
  **bases Gemma 4 y Qwen 3.5** (el 9B es Qwen 3.5 — coincide con el
  `family: qwen35` que Ollama reporta para nuestro blob). MIT.
- Entrenamiento: RL con "self-improving training framework" que
  **optimiza conjuntamente el scaffold y la solución**.
- Reasoning model (`<think>`), tool-calls formato Qwen3 XML/coder,
  contexto 256K, GGUF oficial.
- **Sampling recomendado por el vendor: temp 0.6, top_p 0.95,
  top_k 20.**
- Benchmarks frontier para el 397B (SWE-Bench Verified 82.4%); el 9B
  es el hermano chico de esa receta.

## Lo que esto explica de nuestros datos (3 conexiones)

1. **Los RouteMiss son plausiblemente acoplamiento de scaffold.** El
   modelo se entrenó JUNTO a su scaffold — sus preferencias de ruta
   (resolver `edit_file_basic` vía read+write, el caso que forzó la
   métrica dual del 12-ago) son consistentes con políticas aprendidas
   contra un inventario de tools distinto al nuestro. Es la evidencia
   de producto de la arista MODEL—BENCH que ya documentamos, y el
   pariente industrial de nuestra línea experto-por-motor (ellos hacen
   a escala RL lo que nuestro QLoRA propone en miniatura). También
   dialoga con AutoDesign: ellos optimizan el harness con modelo fijo;
   Ornith optimiza modelo+scaffold juntos — la tercera casilla de esa
   matriz.
2. **Las truncaciones del sweep SC tienen explicación candidata.** Los
   `ModelBackendError` "truncated by token budget" de ornith en
   sc-compaction (4-7 por brazo, 5-7k tokens de salida) encajan con un
   reasoning model pensando largo en tareas duras sin
   `BRAZE_MAX_TOKENS` generoso — el caveat ya documentado para
   qwen3.5-coder aplica a ornith. Afecta a ambos brazos por igual (el
   pareo lo absorbe), pero es varianza evitable en sweeps futuros:
   presupuestar tokens para ornith.
3. **Nuestros resultados de ornith corren a temp 0.2, el vendor
   recomienda 0.6.** Es exactamente la advertencia de
   lourie2026smallscale (nota de ayer): veredictos medidos fuera de la
   frontera tuneada del modelo. El chequeo de sensibilidad que esa
   nota dejó como idea ahora tiene candidato concreto: re-correr UN
   A/B cerrado de ornith (p.ej. el par M1 `move`, el más discordante)
   a temp 0.6/top_p 0.95/top_k 20 y ver si el veredicto flipea. Sigue
   siendo idea de backlog, NO pre-registrada aquí.

## Acciones

1. Backlog: chequeo de sensibilidad ornith@0.6 (idea #3) — barato,
   1 par × 2 sampling points.
2. Al leer el veredicto SC-retention: anotar que las truncaciones de
   ornith son candidato a artefacto de presupuesto de reasoning, no
   de la palanca (pareado, pero declararlo).
3. Si Nitro sube a 32 GB: **Ornith-1 35B-MoE** entra al mapa como
   rival directo de gpt-oss:20b (MoE, receta RL-agéntica, MIT) — junto
   al gemma-4-26B-A4B ya anotado.
4. Wiki/CLAUDE.md: la línea "ornith:9b (dense 9B)" gana el apellido
   real: Ornith-1-9B (DeepReinforce, base Qwen 3.5, RL
   scaffold-conjunto, reasoning) — actualizar cuando toque editar esas
   páginas, no urgente.

## Addendum 2026-08-19: salió Ornith-1.5-9B (HF: ornith-ai/Ornith-1.5-9B)

Cambios relevantes vs 1.0: el self-improvement ahora **genera sus
propias tareas de entrenamiento** (task generation + scaffold +
solución optimizados conjuntamente por RL — el loop completo que
Meta-Harness/AutoDesign hacen solo del lado harness); multimodal
(texto+imagen); mismo template Qwen3.5/XML/thinking (compatible con
braze sin cambios); sampling recomendado igual (0.6 coding — nuestro
0.2 sigue siendo la nota de sensibilidad pendiente); 256K contexto;
GGUF disponible; MIT. Claims fuertes (SWE-bench Verified 70.6 para un
9B) — a verificar con NUESTROS oráculos, no adoptar de la card.

Implicancias: (a) candidato a suceder a ornith:9b en el lineup —
requiere el rito de adopción completo (tool-support check + sweep
discriminante pareado vs 1.0, pre-registrado); (b) la réplica SC ×10
pendiente debe correr sobre 1.0 (el sujeto pre-registrado) — 1.5
sería experimento aparte; (c) un modelo que genera sus propias tareas
cambia la forma del riesgo de contaminación de benchmarks — refuerza
el valor de suites propias con oráculo cargo check y del DBV.

### Detalles de la página de release (ornith.ai/ornith_1_5.html, leída 2026-08-19)

**El reward de generación de tareas es nuestra suite discriminante
convertida en señal de entrenamiento**: R_task = V × D × N donde V =
validez (el scaffold ejecuta y las soluciones son verificables), D =
"frontier difficulty" con **success rate objetivo ~20%** medido por
rollouts, N = novedad semántica. El término D es exactamente el
principio de diseño de discriminating.toml ("tareas cerca de la
frontera del modelo") — ellos lo escribieron en la función de reward.
Y el reward del harness (C × F × H: task alignment, reward fidelity,
**hack resistance**) convierte en señal de RL los dos temas nuestros:
la validez del oráculo (la lección de la suite v1: aserciones vacuas)
y la integridad silenciosa (dsh). Convergencia a nivel de
entrenamiento, no solo de evaluación.

**Rigor de evaluación**: promedian 5 corridas independientes (mejor
que los point estimates de Meta-Harness/AutoDesign; aún sin CIs ni
tests), anti-hacking explícito (git history removido, red apagada),
jueces independientes. **Sus propios benchmarks corren a temperatura
1.0** (Terminal-Bench/SWE-bench; ClawEval a 0.6): el mapa de sampling
queda vendor-evals 1.0 / recomendación coding 0.6 / braze 0.2 — la
nota de sensibilidad se agudiza.

**Claims a verificar con máxima sospecha**: 9B con Terminal-Bench 2.1
= 47.0 usando el harness de Claude Code (compárese: Meta-Harness
reportó Haiku 4.5 a 27.5 con Claude Code en TB-2), GPQA Diamond 86.4
para un 9B, y "matches Gemma 4-31B / Qwen 3.6-35B". O el currículo
auto-generado es un breakthrough real, o hay benchmark-fitting fino —
el rito de adopción con nuestros oráculos es el árbitro correcto, y
ahora el A/B 1.5-vs-1.0 tiene interés científico propio (¿el task
generation loop produce capacidad agéntica transferible?). Familia
completa: 397B MoE (claim: a la par de Opus 4.8 en TB), 35B MoE, 9B
dense, 9B-Mobile cuantizado.
