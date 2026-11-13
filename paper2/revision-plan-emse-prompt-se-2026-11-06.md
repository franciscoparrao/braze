# Plan de revisión del Paper 2 para EMSE PROMPT-SE (2026-11-06)

Origen: review simulado del 2026-11-06 (Reject; A y B Reject, C Major al borde de
Reject), en `~/vault/journals/emse/reviews-generated/2026-11-06_20-31_paper2-prompt-se.md`.
Datos del reanálisis: `docs/paper2-reanalysis-2026-11-06.md`, generado por
`paper2/analysis/reanalysis.py`. Plazo del número especial: 2027-03-01.

## Conclusión

El paper se puede salvar, pero con otra historia. Los datos que ya existen sostienen
una lectura más modesta y más sólida. Para el aporte metodológico del Estudio 2 hacen
falta corridas nuevas en Nitro.

## Lo que muestra el reanálisis (verificado, `docs/paper2-reanalysis-2026-11-06.md`)

1. **Estudio 2: toda la diferencia entre brazos viene de los timeouts.** El baseline
   tuvo 38 timeouts, `empty` 23 y `seeded` 21. Fuera de los pares con timeout, los
   contrastes quedan en 2/2, 0/3 y 5/1 discordantes, todos nulos. Entre las corridas
   sin timeout, el baseline pasa 59/64, más que `empty` (68/79) y `seeded` (72/81). El
   +13 «falso positivo» y el +9 del control son el mismo fenómeno: el baseline corrió
   1,7 veces más lento por ronda (100 s frente a 58-61 s de mediana), con brazos en
   secuencia y una suspensión del nodo durante el barrido.
2. **Réplica 9B: el playbook no reduce la corrección; alarga las trayectorias.**
   - Corrección funcional (archivo + `cargo check` + sin timeout): `original` 17→15,
     `loop` 12→13, `move` 11→3. Ninguna sobrevive a Holm; `move` queda en 0,056.
   - Rondas: el playbook **agrega** 0,50, 1,55 y 1,75 rondas; `loop` (p Holm < 0,001)
     y `move` (0,049) son significativas.
   - Tokens netos: +2.709, +5.794 y +6.635 por tarea, las tres significativas con Holm.
   - El «daño» de la versión actual eran fallas del tope de 12.000 tokens del oráculo:
     0→12 en `loop` y 4→13 en `move`.
3. **Estudio 1: la etiqueta de `original` no se sostiene, pero su ΔR sí.** Sin ayuda,
   `original` es la tarea que peor resuelve el modelo (14/20, 6,70 rondas). Su ΔR =
   +0,95 sobrevive a Holm (p = 0,027) y supera su propio break-even (0,75). En `loop` y
   `move`, ΔR no es significativo.
4. **Break-even por tarea:** 0,75, 0,96 y 1,16 rondas (20B); 1,01, 1,68 y 1,77 (9B).
   No es «≈0,9-1,2» para todas.
5. **ΔT no es el costo de inyección, y no se puede descomponer con estos datos.** Los
   «299 tokens» del playbook son la estimación del bench (caracteres / 4), no un
   conteo del tokenizador de cada modelo. El ΔT medido (184-268 en el 20B, 398-582 en
   el 9B) mezcla inyección y cambio de trayectoria. Reportarlo como diferencia de
   entrada por ronda, sin atribuirlo a la inyección. (Resuelto el punto 11.)
6. **El barrido 0/190 no puede medir el stack:** 95/95 aprobadas. Las trayectorias sí
   divergen: 56 de 190 pares tienen distinta huella de resultado.
7. **El Estudio 1 tiene 22 archivos de barrido.** El paper reporta uno (`r20-moveB`,
   140 corridas, huella `a5fccc8f`), que no está commiteado como suite. No hay regla
   de inclusión escrita.

## La historia que los datos sostienen (decisión del autor)

Propuesta, para que la evalúes:

- **Tesis:** inyectar memoria en el prompt tiene un costo por ronda que la condición de
  amortización hace explícito. Con un playbook humano, el 20B ahorra rondas solo en una
  tarea; en el 9B, la memoria alarga las trayectorias y multiplica los tokens sin
  cambiar la corrección.
- **Se retira:** «el fallo pasa de económico a conductual»; «la tarea que el modelo ya
  conoce»; «el piso de ruido es diseño de suite, no del stack»; «falla de break-even por
  3-6×» como cifra confirmatoria.
- **Se reformula el aporte metodológico del control de mismo prompt.** Con los datos
  actuales lo que se puede decir es que detectó un artefacto a nivel de brazo
  (rendimiento y timeouts), no que mida ruido de celda. Si se quiere sostener la versión
  fuerte, hace falta la corrida intercalada de la sección C.

## A. Hecho (2026-11-06)

- [x] `paper2/analysis/reanalysis.py`: script único que regenera los estadísticos.
- [x] `docs/paper2-reanalysis-2026-11-06.md`: salida del script.

## B. Con los datos existentes (no requiere Nitro)

1. [x] Definir el éxito como corrección funcional y reportar aparte los topes de tokens y
   rondas. Declarar todas las aserciones del oráculo y que el Estudio 1 no corrió
   `cargo check`. (Críticos 2 y 3)
2. [x] Reescribir la réplica: alarga trayectorias y cuesta tokens; la corrección no cambia.
3. [x] Quitar «saturated/known/fresh» o redefinirlo con los brazos sin memoria. Con tres
   tareas, no afirmar una anti-correlación.
4. [x] Break-even por tarea con su intervalo; presentar la ecuación (1) como descomposición
   contable.
5. [x] Holm en todas las familias de endpoints, incluida la tasa de éxito de la réplica y
   los tokens netos.
6. [x] Estudio 2: reportar timeouts y rendimiento por brazo, el orden de los brazos y la
   suspensión del nodo. Reportar el análisis sin pares con timeout. Retirar la
   atribución «suite, no stack» y el 0/190 como prueba.
7. [x] SC-retention: reportar la réplica rechazada (5/80 frente a 8/80, p = 0,55, +18 %
   de tokens) o quitar el párrafo.
8. [x] Hacer consistente el estado de pre-registro (Estudio 1 exploratorio en todo el texto;
   su plan de Holm es posterior a los datos).
9. [x] Apéndice con los artefactos textuales: playbook renderizado, sección de memoria del
   proyecto y system prompt, con conteo de tokens.
10. [x] Inventario de barridos del Estudio 1 con regla de inclusión; commitear la suite
    exacta (huella `a5fccc8f`) o declarar que no se puede.
11. [x] ΔT residual: investigado; `memory_tokens` es caracteres / 4 (ver punto 5).
12. [x] Relacionado: Memp, ACE, Agent Workflow Memory, estudios de AGENTS.md/CLAUDE.md
    (arXiv 2509.14744, 2511.12884, 2602.11988, este último pendiente de verificar),
    Ouyang et al. (no determinismo, TOSEM). Pasar todas por `/verify-refs`.
13. Figura 1: barras de incertidumbre en 2D; agregar la réplica.
14. [x] Datos de ejecución: hash del GGUF del Estudio 2, versiones de Ollama (0.30.7 en el
    Estudio 1, 0.32.1 en la réplica), sesiones de diagnóstico o declarar que no están.

## C. Requiere Nitro (corridas nuevas)

1. **Estudio 2 intercalado:** los tres brazos, con orden aleatorio por (tarea,
   repetición), registrando el rendimiento por corrida. Pre-registrar antes:
   hipótesis, análisis con timeouts como censura y regla de validez.
2. **Control de mismo prompt en la réplica** (opcional): un brazo A/A intercalado.
3. **Piso de ruido en el mismo stack:** LocalBackend, tope de 900 s, suite
   discriminante, midiendo divergencia de huellas y no solo pass/fail.

Si C.1 no se hace, el paper va con la historia reformulada de la sección B y el
control de mismo prompt queda como herramienta de detección de artefactos.

## D. Empaque (al final)

- Tag anotado nuevo para EMSE y versión nueva en Zenodo que incluya todo lo citado.
- Cover letter: actualizar con el encaje y la historia nueva.
- Repetir `/paper-review-emse` en modo blind antes de enviar.

## Estado al 2026-11-06 (fin de la primera pasada de la sección B)

Hecho en `paper2/main.tex` (compila limpio, 21 págs., sin cajas desbordadas): puntos
1-10 y 14 de la sección B, con todas las cifras tomadas de
`docs/paper2-reanalysis-2026-11-06.md`. El punto 14 queda parcial: se declaran las
versiones de Ollama y que las sesiones no están archivadas; falta el hash del GGUF del
Estudio 2. Pendientes de B: 12 (literatura nueva, requiere `/verify-refs`) y 13 (figura 1
con incertidumbre y la réplica). Pendientes de D: tag anotado, versión nueva en Zenodo
(marcada con `\todo` en el manuscrito) y actualizar la cover letter.
