# Ranking SML por LocalBackend: gemma-4-12B vs gpt-oss:20b — 2026-07-21

**Pregunta**: ¿gpt-oss:20b es superior al gemma-4-12B recién instalado?
Suite `default.toml` (19×3=57 por brazo), seed 42, Nitro. Configs por
viabilidad (no idénticas — divulgado): gpt-oss CPU puro; 12B con 14/48
capas GPU (VRAM compartida con otras cargas) y timeout 360s (los 180s
default censuraban la mitad de sus corridas; el primer intento con 12
capas/180s quedó como procedencia: 29/57 timeouts).

## Resultado

| | gpt-oss:20b (CPU) | gemma-4-12B (GPU 14/48, 360s) |
|---|---|---|
| pass rate | **57/57 (100%)** [94,100] | 30/57 (53%) [40,65] |
| pass^3 | **100%** | 52.6% |
| timeouts | 0 | **26/57** |
| pass condicional (sin timeout) | 100% | **30/31 (97%)** ⚠️ ver corrección |
| schema_fail / rescues | 0 / 0 (por diseño Harmony) | 0 / 71 |
| avg por tarea | 41s | 175s |

McNemar pareado: solo-12B=0, solo-oss=27, **p=1.5e-08** — decisivo.

## Lectura

1. **Operacionalmente en Nitro, gpt-oss:20b es superior sin discusión**
   — pero por throughput, no por inteligencia: MoE de 3.6B activos vs
   denso de 12B es ~4-8× de velocidad, y el workload agéntico es
   multi-ronda y latency-bound. Duplicar el timeout apenas movió la
   censura (29→26 timeouts): el 12B denso no cabe en el presupuesto de
   tiempo de este hardware con offload parcial.
2. **En capacidad pura están casi empatados**: 97% condicional del 12B
   (un solo fallo real en 31 corridas completadas: una aserción de
   edit_file) vs 100% — indistinguibles a este n. El 12B "sabe" hacer
   las tareas; no alcanza a hacerlas.
   > ⚠️ **Esta lectura quedó refutada el 2026-07-25 — ver § Corrección.**
   > El 97% condicional era un artefacto de **censura informativa**: las
   > corridas que se caían por timeout eran justamente las que iban a
   > fallar. Al completar más, la capacidad medida BAJA.
3. **La lección generaliza la tesis SML del proyecto**: en hardware
   modesto, los parámetros ACTIVOS dominan la utilidad agéntica. El
   campeón local sigue siendo gpt-oss:20b — ahora con su mejor forma de
   correr: el LocalBackend Harmony (57/57, pass^3=100%, el mejor número
   del proyecto; mismo modelo vía Ollama: 98.9% n=95).
4. El brazo gpt-oss fue además la primera suite completa del camino
   Harmony — verificación a escala de la Fase 2b con puntaje perfecto.

Datos: `sweep-rank-oss.json`, `sweep-rank-12b-v3.json` (v1 censurado y
v2 abortado por OOM quedan en Nitro como procedencia).

---

## Corrección (2026-07-25): el 97% condicional estaba inflado

Al re-correr el 12B con el auto-fit de capas (palanca #1,
`docs/local-backend-design-2026-07-20.md`) se completaron **34 corridas en
vez de 31**, y la lectura de arriba no sobrevivió.

| | v3 (14 capas, 21-jul) | auto-fit (33 capas, 25-jul) |
|---|---|---|
| pass rate | 30/57 (52.6%) | 27/57 (47.4%) |
| timeouts | 26/57 | 23/57 |
| **pass condicional** | **30/31 (96.8%)** | **27/34 (79.4%)** |

Lo decisivo no es el promedio sino **qué pasó con las corridas rescatadas**:
de los 5 timeouts del v3 que el auto-fit dejó terminar, **4 fallan** (3
`assertion_files`, 1 `assertion_text`) y solo 1 pasa.

**La censura era informativa, no aleatoria.** Las corridas que se caían por
timeout eran desproporcionadamente las que iban a fallar — coherente con el
mecanismo: una tarea que el modelo no está resolviendo bien gasta rondas
extra, y esas rondas son las que agotan el presupuesto. Condicionar en
"completó" seleccionaba a favor de los casos fáciles y **sobreestimaba la
capacidad**.

Consecuencias para lo que este doc concluyó:

- **Cae** la frase "el 12B *sabe* hacer las tareas; no alcanza a hacerlas".
  No se puede sostener con estos datos: cuando alcanza, falla más de lo que
  el 97% sugería.
- **Cae** "en capacidad pura están casi empatados". A 79.4% condicional
  contra el 100% de gpt-oss, ya no son indistinguibles.
- **Se mantiene, y reforzada**, la conclusión principal: gpt-oss:20b es
  superior en este hardware. El argumento de throughput sigue en pie y ahora
  además hay un gap de capacidad.
- **Se mantiene** la lección de la tesis SML (los parámetros ACTIVOS dominan
  la utilidad agéntica en hardware modesto).

**Lección de método, que es lo que más vale de esto:** un pass rate
condicional sobre datos censurados es un estimador sesgado salvo que la
censura sea independiente del resultado — y acá el mecanismo mismo predice
que no lo es. En sweeps con timeouts, reportar el condicional **sin** el
chequeo de qué pasa al levantar la censura es afirmar de más. El chequeo
barato: subir el presupuesto (o acelerar el runtime) y mirar si las corridas
rescatadas pasan o fallan.

**Nota sobre velocidad**: la censura bajó poco (26 → 23 timeouts, y 9 → 8 en
el subconjunto de 19 tareas del brazo con placement medido). El trabajo
posterior del 25-jul —auto-fit + KV placement por medición— mejoró el
walltime de forma sustancial en las tareas que completan (15.0s vs 29.2s de
la regla vieja, sobre las 9 comparables), pero **no alcanza a sacar al 12B
denso del techo de tiempo de este hardware**. Eso no contradice nada de
arriba: lo confirma por otra vía.

Datos de la corrección: `sweep-rank-12b-autofit.json`,
`sweep-12b-medido.json`, `sweep-12b-14capas-hoy.json`,
`sweep-12b-14capas-kvgpu.json` (todos en Nitro).
