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
