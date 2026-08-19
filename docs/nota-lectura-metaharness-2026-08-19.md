# Nota de lectura: Meta-Harness (arXiv 2603.28052, 30-mar-2026)

**Lee, Nair, Zhang, Khattab, K. Lee, Finn — "Meta-Harness: End-to-End
Optimization of Model Harnesses"** — Stanford IRIS + MIT (Khattab/DSPy)
+ KRAFTON. PDF en `docs/2603.28052v1.pdf`. Leída 2026-08-19. Clave
sugerida: `lee2026metaharness`. **Es el antecesor directo (5 meses) del
AutoDesign de Meituan** (`docs/nota-lectura-autodesign-2026-08-15.md`)
— la genealogía del meta-harness queda: Meta-Harness (mar) →
AutoDesign (ago) → el gap que braze puede llenar.

## Qué es

Outer-loop que busca sobre CÓDIGO de harness con un proposer agéntico
(Opus-4.6, max reasoning) que accede vía filesystem al historial
COMPLETO sin comprimir — código fuente, scores y trazas de ejecución de
todos los candidatos previos, inspección selectiva. Su argumento
central contra los text-optimizers (OpenEvolve, TTT-Discover): esos
comprimen el feedback (scores escalares, resúmenes cortos) y por eso
pierden; darle al proposer acceso rico a la experiencia previa iguala
a los mejores en 0.1× las evaluaciones y los supera por >10 puntos.
Resultados: clasificación +7.7 sobre ACE **usando 4× menos tokens de
contexto**; un harness descubierto para RAG-math generaliza +4.7 sobre
5 modelos held-out; TerminalBench-2: 76.4% con Opus 4.6 (#2 del
leaderboard) y **37.6% con Haiku 4.5, superando a Goose (35.5) y a
Claude Code (27.5)**.

## Los tres datos que le sirven a braze directo

1. **La ganancia es MAYOR en el modelo más débil** (Haiku: +2.1 sobre
   el mejor harness manual; en Opus el margen es menor y ForgeCode
   no-reproducible les gana). Evidencia independiente, a escala
   frontier-adyacente, de la curva harness-vs-escala del Paper 1.
2. **La economía de tokens como co-objetivo alcanzable**: su mejor
   harness de clasificación gana precisión CON 4× menos contexto. En
   términos del Paper 2: el buscador encontró solo las dos salidas de
   la Ec. (1) — bajó ΔT y subió el rendimiento a la vez. Nosotros
   preciamos la frontera; ellos muestran que un optimizador puede
   cruzarla.
3. **El proposer usa retrieval-on-demand sobre historial completo** —
   filesystem como memoria, inspección JIT, cero inyección fija. Es
   exactamente el perfil de costo "fuera del prompt" que el Paper 2
   señala como la salida ΔT — aplicado al meta-nivel. Resonancia
   fuerte, citable en el follow-up.

## El gap que confirma nuestro ángulo (ahora con dos datapoints)

Selección de candidatos: **"solely based on search-set performance"**
— estimaciones puntuales, sin gates estadísticos, sin piso de ruido,
igual que AutoDesign 5 meses después. Y en TerminalBench-2 **buscan y
evalúan sobre las MISMAS 89 tareas** (lo declaran y justifican como
"discovery problem" + auditorías manuales/regex de leakage — honesto
pero débil: sin split, sin estadística de la mejora). Ninguno de los
dos sistemas líderes de la línea maneja el ruido: la maquinaria de
braze (pre-registro, McNemar+Holm, piso in-sweep/control mismo-prompt,
MDE, secuencial anytime-valid) sigue siendo exactamente lo que esta
línea no tiene — y el pm-ab (+13 falso que un gate puntual habría
promovido) es la demostración concreta. El ángulo "meta-harness under
noise" pasa de plausible a documentadamente vacante.

## Otros apuntes

- Cita [47] de su intro: "cambiar el harness produce un gap de 6× en
  el mismo benchmark" — perseguirla para el follow-up (candidata a
  reemplazar/acompañar el framing de apertura).
- Claude Code 27.5% en TerminalBench-2/Haiku como harness genérico vs
  37.6% del descubierto: los harnesses generalistas pagan su
  generalidad — pariente del hallazgo RouteMiss/acoplamiento (nota
  Ornith-1).
- Contraste de acceso a experiencia: Meta-Harness (historial crudo +
  inspección selectiva) vs AutoDesign (registro L curado) — dos
  respuestas al mismo problema de memoria del optimizador; el Paper 2
  diría que la de Meta-Harness es la del perfil de costo correcto.

## Acciones

1. `lee2026metaharness` a la cola del bib del follow-up, junto a
   luo2026autodesign / li2026loopsbench / wang2026compint /
   lourie2026smallscale. `/verify-refs` en su momento.
2. El párrafo de Related Work del follow-up gana estructura de arco:
   Meta-Harness formaliza y muestra factibilidad (acceso rico a
   experiencia), AutoDesign agrega taxonomía de componentes y gate
   train/dev, braze aporta la maquinaria de aceptación estadística
   para el régimen ruidoso/SLM que ambos omiten.
3. Perseguir la cita [47] (gap de 6×) cuando se arme el bib.
4. Actualizar mención en la nota AutoDesign: el "ángulo publicable
   propio" ahora tiene DOS sistemas líderes con el mismo gap (hecho en
   esta nota; la de AutoDesign queda como está, esta la referencia).
