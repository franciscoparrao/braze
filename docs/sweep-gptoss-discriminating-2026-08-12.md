# Techo de gpt-oss:20b en la suite discriminante — lectura PARCIAL (dos infra distintas)

Fecha: 2026-08-12
Datos: `docs/sweep-gptoss-discriminating-2026-08-11-v2.json` (73 filas de
102; el sweep abortó en la tarea 24). Suite `discriminating.toml` (34
tareas, familias escalera_*), gpt-oss:20b vía Ollama en Nitro, reps 3,
seed 42, timeout 600s.

## Advertencia de lectura: esto NO es el techo limpio

Tres corridas, tres lecciones de infra:
- **v1** (KEEP_ALIVE=2m): 0/67, el modelo se evictaba en los huecos de
  cargo → reload storms. INVÁLIDO. Fix: KEEP_ALIVE=-1.
- **v2** (este): el fix funcionó —generación a 15 t/s, floors 100%— pero
  emergieron DOS infra distintas del keep-alive, que contaminan justo las
  familias duras (las que definen el techo):
  1. **Wall-clock 600s corto para contexto grande**: `mover_*`/`borrar_30`
     hacen 3-7 rondas sobre archivos grandes; cada ronda paga prefill CPU
     (~140s), y 4×140≈560s agota el presupuesto ANTES de que el modelo
     converja o se rinda. El "timeout" ahí conflaciona "no converge"
     (capacidad) con "necesita más de 600s" (régimen). Es el hallazgo
     round-economics en vivo.
  2. **Errores de transporte → circuit breaker → aborto**: 5
     `error decoding response body` en respuestas grandes abrieron el
     breaker en la tarea 24; las tareas 25-34 (las multi-archivo, el TOPE
     real de la curva) **nunca corrieron**. Nitro al límite de RAM (202MB
     libres, gpt-oss 14GB en 14GB) — el argumento de subir a 32GB.

## Lo que SÍ se puede concluir (familias limpias, infra=0)

| familia | resultado | lectura |
|---|---|---|
| **piso** (floor) | fix_dos_errores 3/3, localizar_editar 3/3 | **100% — sanity check pasa**, el fix es real |
| **localizacion** | editar_lote 3/17/34 todas 3/3 | **100%** — localizar y editar en archivo grande NO es problema |
| **consistencia** | renombrar 2/5/10 usos: 2/3, 2/3, 3/3 | **alto y plano** — la escalera de consistencia NO cae (10 usos = 3/3) |
| **preservacion** | vecino 0/1/2: 2/3, 3/3, 3/3 | **alto** — preservar el vecino al editar se sostiene |
| **escalera_errores** (bajo) | arreglar_1: 3/3, arreglar_2: 1/3 | arreglar_1 sólido; arreglar_2 ruidoso (1/3) |
| **escalera_borrado** (bajo-medio) | borrar_2: 1/3(!), 6/12/20: 2/3 c/u (cap2) | borra hasta 20 funciones ~67%; borrar_2 anómalo (1/3, revisar) |

## Lo que queda OBSCURO (contaminado o no corrió)

- **escalera_movimiento** (mover_1/3/8): **0/3 todas, por timeout+transport**.
  El modelo hace rondas y genera ediciones pero no converge en 600s, y 2
  de 9 fueron transport-error. Es CONSISTENTE con el hallazgo roam
  (gpt-oss falla los movimientos de bloque) PERO no se puede afirmar
  "techo de capacidad" vs "régimen de wall-clock" con estos datos.
- **borrar_30, arreglar_3**: todas infra (timeout/transport). Sin lectura.
- **tareas 25-34** (mover_item, cambio_coordinado_dos_archivos,
  mover_almacen_completo, tres_archivos_coordinados, contar_metodos,
  reportar_campo, localizar_funcion, editar_lote_25/39): **NUNCA
  CORRIERON** (breaker abierto). El tope multi-archivo de la curva —lo
  más interesante— sigue sin medir.

## Veredicto honesto

gpt-oss:20b es **sólido en la mitad medible de la suite**: localización,
consistencia, preservación, error-fixing chico y floors, todo ~100% o
alto y plano — o sea la suite discriminante NO lo satura por el lado
"fácil-medio", que ya es información (default.toml sí lo saturaba). Pero
**el techo real —movimientos de bloque y coordinación multi-archivo— NO
se midió limpio**: las familias que lo definen cayeron por wall-clock
corto + transport errors + aborto del breaker.

Esto NO es un fracaso del ejercicio: es el descubrimiento de que medir el
techo de gpt-oss en tareas de contexto grande exige (a) wall-clock por
tarea ≥900s (round-economics ya lo predijo: 300s oscila, 900s estabiliza),
(b) mitigar los transport errors (Nitro a 32GB, o investigar si es
timeout del cliente HTTP de braze en prefills largos), y (c) re-correr las
9 tareas del tope en aislamiento. La suite discriminante FUNCIONA como
discriminador; la infra de Nitro es la que no aguanta el tope.

## Próximo paso (a decidir, no lanzado)

- Re-correr SOLO tasks 24-34 + las familias mover/borrar_30 con
  `--task-timeout-secs 900` y un tope de rondas explícito bajo
  (`+ablate:max-iterations=N`) para que la NO-convergencia salga como
  `assertion_max_rounds` (señal de capacidad limpia) en vez de timeout de
  wall-clock. Investigar el transport error antes (¿cliente HTTP? ¿RAM?).
- O aceptar la lectura parcial: gpt-oss no se satura por el lado
  fácil-medio de discriminating, y el tope pesado queda como "requiere
  infra que Nitro hoy no da".
