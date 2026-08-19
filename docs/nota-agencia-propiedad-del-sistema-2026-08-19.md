# Nota conceptual: la agencia es propiedad del sistema, no del modelo

Fecha: 2026-08-19. Destino: discusión del follow-up (junto al arco
Meta-Harness → AutoDesign → braze). Disparador: el framing de
Ornith-1.5 ("un modelo de lenguaje capaz de llevar a cabo acciones
agénticas") observado por el autor.

## El claim y su tensión interna

Los releases de modelos RL-agénticos atribuyen la agencia a los pesos.
Pero el material de Ornith-1.5 se contradice solo: (1) entrenan
CONJUNTAMENTE task-generation + scaffold + solución, con reward propio
para el scaffold (C×F×H) — el andamiaje es ciudadano de primera clase
de lo que optimizan; (2) sus benchmarks reportan cifras POR HARNESS
(TB2.1 con el harness de Claude Code, SWE-bench con OpenHands, DeepSWE
con otro) — si la agencia viviera en el modelo, esas columnas serían
una. La cita [47] de Meta-Harness cuantifica el punto: mismo modelo,
distinto harness, gap de 6×.

## La formulación defendible

"Modelo capaz de acciones agénticas" es cierto en sentido estrecho —
las capacidades (emitir tool calls, sostener loops multi-ronda) están
en los pesos — pero la **atribución de rendimiento** pertenece al
sistema J(θ, H). Quien reporta J como propiedad de θ suma
silenciosamente el trabajo del harness de otro. El mapa de la línea:
AutoDesign/Meta-Harness optimizan H con θ fijo; Ornith optimiza θ con
su H de entrenamiento; braze mide la INTERACCIÓN θ×H con estadística —
la casilla que nadie más ocupa.

## Evidencia propia del término de interacción (grande y de signo no obvio)

- Matriz 4 brazos: mismo modelo, planner −22pp / lead +21pp.
- RouteMiss (métrica dual, 67ab6d5): ornith y gemma traen preferencias
  de ruta de SUS harnesses de entrenamiento incrustadas — en un
  harness ajeno aparecen como "ruta equivocada" con logro funcional.
- SC-retention: el cumplimiento de constraints es producto conjunto
  (ornith los honra SOLO cuando el harness los preserva verbatim).
- Edit-fence: la modalidad entrenada (JSON) es parte de la identidad
  del modelo; imponerle otra la daña.
- Dialecto gemma-4 (hallazgo D3): format tax por construcción.

## Lo que hay que concederle al framing (la frontera se corre de verdad)

El RL agéntico destila hacia los pesos cosas que eran andamiaje:
formatos de tool-calling, hábitos de verificación, políticas de
reintento. Ornith-1.5 es el caso extremo (aprendió CON su scaffold y
generando su propio currículo). La distinción modelo/harness no es
falsa, pero es cada vez menos un límite de implementación y cada vez
más un eje de atribución que hay que medir.

## La predicción medible que sale de aquí

**Acoplamiento como costo de generalización**: un modelo co-entrenado
con su scaffold debería rendir desproporcionadamente peor en harnesses
ajenos que uno entrenado neutro — y dejar huella observable (tasa de
RouteMiss / brecha passed vs passed_strict mayor). El RouteMiss de
ornith-1.0 ya lo insinuó; el 2×2 pre-registrado
(`docs/hypothesis-2026-08-19-ornith15-transfer-2x2.md`) es su primera
medición dirigida: si 1.5 (más co-adaptado) muestra MÁS RouteMiss que
1.0 bajo nuestro harness, el acoplamiento-como-costo gana su primer
número.

## Para el follow-up

Párrafo de discusión: agency-as-system-property + la tabla de
evidencia de interacción + la predicción de acoplamiento con el
resultado del 2×2 (corra como corra). Se engancha con: benchmark-
capacidad ≠ confiabilidad agéntica (tema del proyecto), suites
privadas como auditoría resistente a contaminación, y el arco
meta-harness (quién optimiza qué con qué fijo).
