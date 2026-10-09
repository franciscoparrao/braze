# Calibración del revisor local de manuscritos (2026-10-08/09)

**Conclusión.** El revisor local (`scripts/review/run_review.py`, gpt-oss:20b
en Nitro, sin salir de la red local) llega a un recall de unos 3 de 9
hallazgos de una reseña de referencia y acierta el veredicto. Filtra bien
los falsos positivos de **ausencia** ("falta X"), pero no los de
**interpretación**. Sirve como borrador que detecta huecos verificables,
no como juez. El techo lo pone el modelo, no el harness: con el paper
entero leído y anclado, la síntesis no mejoró.

## Diseño de la prueba

- **Manuscrito:** Paper 2 (`paper2/main.tex`, 1.189 líneas), sometido a IST.
- **Criterios:** calibración IST del vault (`criterios-ist.md`).
- **Referencia dorada:** la reseña de frontera del 2026-09-08
  (`~/vault/journals/ist/reviews-generated/2026-09-08_11-00_paper2-amortization.md`),
  veredicto Major Revision, con 5 críticos (C1-C5) y 5 menores (M1-M5).
  M1 (abstract no estructurado) se excluye: el `main.tex` actual ya tiene
  el abstract estructurado. Rúbrica efectiva: 9 hallazgos.
- **Modelo:** gpt-oss:20b vía Ollama 0.32.1 en Nitro, `num_ctx` 32768.

## Cuatro calibraciones

| # | Diseño | Lectura del paper | Recall (de 9) | Veredicto | Qué falló |
|---|---|---|---|---|---|
| 1 | Un turno agéntico; el modelo navega con `read_file` | completa, luego borrada por compactación | 0 | Major (por azar) | reseña de un paper inexistente |
| 2 | + notas a archivo (`append`), perfil propio, chequeo de anclaje | 1 de 9 bloques | ~0 | Major | el modelo no sostiene el loop |
| 3 | map-reduce dirigido por el script, sin tools | 7 de 10 bloques | 2 + 2 parciales | Major | 3 bloques truncados (incluido el de caching, C2) |
| 4 | = 3 con los 3 bloques recuperados | 10 de 10 | 3 parciales | Major | la síntesis no usó el material nuevo |

### 1. Compactación por conteo de eventos

El modelo leyó las 1.189 líneas en las rondas 0-8. Después la compactación
se disparó por conteo de eventos (umbral 40) y el digest solo conservó
`read_file(main.tex)`. El modelo dio vueltas releyendo y emitió una reseña
genérica de "herramientas de code review".

Causa del umbral 40: `max_tokens` 16384 del perfil operador más el system
prompt dejaban ~11k tokens de presupuesto, el escalado del umbral quedaba
en ×1, y el paper (~16k tokens) no cabía de todas formas.

Arreglos (commit `0fc7ee6`): `write_file` con `append`; el digest de
compactación nombra los archivos escritos; perfil `scripts/review/profile`
(compactación por tokens, sin instrucciones globales ni hooks, sin shell
ni red); chequeo de anclaje.

### 2. El modelo no sostiene la navegación

Con notas durables y un presupuesto sano, gpt-oss:20b releyó bloques,
chocó con la caché de llamadas repetidas, escribió notas solo de L1-150 y
cerró el turno con su razonamiento filtrado como respuesta. La reseña
salió del abstract.

Arreglo (commit `e57e710`): el recorrido lo dirige el script. Una corrida
por bloque de 120 líneas numeradas (`L123| …`) en el prompt, sin tools.
Eso eliminó también la corrupción de `\times` en argumentos JSON.

### 3. Truncamiento no determinista

Tres bloques (Related Work, tablas de resultados, Discussion) agotaron los
6.144 tokens de salida en los dos intentos. Al re-correrlos en la
calibración 4, los tres salieron bien al primer intento (el bloque 3, con
20/20 anclados): dependía del muestreo. El script ahora reintenta con
12.288 tokens y luego en dos mitades.

### 4. El juicio, no la lectura

Con 164 de 201 notas ancladas y el material de caching en las notas, la
síntesis no lo usó (C2 sigue sin aparecer) y repitió el falso positivo
"faltan intervalos de confianza" citando una línea que tiene un IC
(L640 `95% CI [0.25, 1.65]`).

## Puntuación por hallazgo (síntesis 3 y 4)

| Gold | Síntesis 3 | Síntesis 4 |
|---|---|---|
| C1 scope IST / práctica SE | ✗ | ✗ |
| C2 métrica bajo prefix/KV caching | ✗ (bloque perdido) | ✗ (estaba en las notas) |
| C3 headline sobre piloto exploratorio | parcial | parcial |
| C4 piso de ruido: suite vs serving | parcial | parcial |
| C5 mecanismo desde inspección informal | parcial | ✗ |
| M2 sin power/MDE (n=20) | ✓ | parcial |
| M3 procedencia de ornith:9b | ✗ | ✗ |
| M4 bibliografía | ✗ (el map no ve el `.bib`) | ✗ |
| M5 A→B→H sobre-afirmado | ✗ (lo puso como fortaleza) | ✗ |

## El checker de anclaje

`check_anchors.py` exige `L123` y una frase exacta del manuscrito cerca de
esa línea. Rechazó la reseña alucinada de la calibración 1 (0/4) en todas
sus versiones. Tuvo sesgos propios que se corrigieron sin cambiar ninguna
reseña:

- El modelo cita con tipografía Unicode (`‑` U+2011, `×`, `≈`, `α`) donde
  el `.tex` tiene ASCII o comandos. Plegar ambos lados llevó la síntesis 3
  de 3/10 a 8/10 anclados.
- Formatos de la síntesis 4: anclas en sub-viñetas y citas entre comillas
  tipográficas. Ahora una sub-viñeta es parte del ítem y valen comillas.

Los rechazos que quedan son paráfrasis reales del modelo.

## Verificación independiente por issue (etapa 4)

Cada issue pasa por una corrida aparte que ve solo la crítica y las líneas
citadas (±15) y puede usar `grep`/`read_file` sobre el manuscrito; no ve
las notas ni el resto de la reseña. Responde SUPPORTED, PARTIAL o
UNSUPPORTED; los UNSUPPORTED se eliminan. Es el mecanismo del pre-registro
`docs/hypothesis-2026-09-02-independent-acceptance.md` (Harness-of-Harness):
el aceptador no ve la traza de producción.

| | Síntesis 4 | Síntesis 3 (v1) | Síntesis 3 (v2: reintento + regla de contradicción) |
|---|---|---|---|
| Falsos positivos de ausencia | 2/2 eliminados | "no CIs" PARTIAL (cita el abstract, defendible) | "no CIs" SUPPORTED |
| Falsos positivos de interpretación | — | "contradictorio" SUPPORTED, "overclaiming" SUPPORTED | "contradictorio" PARTIAL, "overclaiming" UNSUPPORTED |
| Críticas del gold (C3, C4) | conservadas | conservadas | conservadas |
| Eliminaciones discutibles | 0 | 0 | 1 ("power": la crítica atribuía al paper una afirmación que no hace) |
| Sin veredicto (`NONE`) | 2/9 | 2/10 | 1/10 |

- **Verificado:** entre v1 y v2, 4 de 10 veredictos cambiaron. Una parte se
  debe al prompt y otra al muestreo; con una corrida por issue no se
  separan.
- **Verificado:** los `NONE` son fallos de uso de tools (argumentos de una
  tool call emitidos como texto; razonamiento filtrado sin respuesta), no
  juicios. Con un reintento bajaron de 4/19 a 1/10.
- **Inferido (probable):** la independencia ayuda cuando la crítica se puede
  comprobar con una búsqueda. Para críticas de lectura, el verificador es
  un juez del mismo nivel que el revisor: la independencia no agrega
  capacidad.

## Uso recomendado

1. Ejecutar
   `python3 -I scripts/review/run_review.py MANUSCRITO.tex CRITERIOS.md OUTDIR --journal "…"`.
2. Leer `OUTDIR/review-final.md` como lista de huecos candidatos, no como
   veredicto.
3. Revisar `OUTDIR/verification.md`: los issues eliminados y su evidencia.
4. Contrastar cada issue contra el manuscrito antes de usarlo.

Costo: ~2 h en Nitro para un paper de ~1.200 líneas (10 bloques de map de
~5-15 min, síntesis y ~10 verificaciones de ~1-2 min).

## Pendientes

- Voto de mayoría en la verificación (3 verificadores, eliminar con 2/3
  UNSUPPORTED) para la inestabilidad de los veredictos.
- Pasada de reduce por criterio de la revista para el recall de C1/C2.
- El map no ve el `.bib` (M4 queda fuera por construcción).
- Hallazgo de harness: una respuesta final truncada por `max_tokens` no
  persiste texto parcial en el rollout.
