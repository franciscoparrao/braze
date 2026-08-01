# Decisión: la familia `BRAZE_LOCAL_*` es una capa env-only deliberada

Fecha: 2026-08-01. Cierra el ítem L-1 de `docs/AUDITORIA-2026-07-v9.md`
(Paquete 2). Formato según `docs/research-discipline-framework-2026-07-16.md`
§ Gate 4.

```text
Decision: documentar la familia BRAZE_LOCAL_* (21 vars) + BRAZE_VERIFY_COMMAND
  como capa env-only de tuning de despliegue, y registrarla completa en la
  metadata de cada sweep (metadata.local_env). NO promoverla al config file.
Evidencia: v9 L-1 lista tres daños. (a) recetas como ristras de exports:
  real, pero esas recetas son por-máquina (capas GPU de la RTX 3050 de
  Nitro no significan nada en otra parte) — un config file compartido las
  fosilizaría fuera de contexto. (b) el bench no puede ablacionarlas por
  fila: promoverlas al config NO lo arregla — la maquinaria +ablate:
  necesita soporte por knob de todos modos. (c) el warning de clave
  desconocida no las conoce: cierto, y el doc de KNOWN_OVERRIDE_KEYS ahora
  las declara explícitamente como no-pertenecientes.
Metricas: metadata.local_env aparece en todo sweep con la familia activa;
  test de que captura el tier y NADA más (una API key en el ambiente no
  debe viajar a un JSON que se commitea a repo público).
Scope donde aplica: los 22 knobs actuales de despliegue del LocalBackend
  y la palanca de verificación.
Scope donde no aplica: cualquier knob que describa al AGENTE y no al
  despliegue (presupuestos de turno, ventana táctica, planner/lead) sigue
  perteneciendo al config file. Si un knob local necesita ergonomía de
  config, se promueve INDIVIDUALMENTE, no la familia entera.
Riesgos: el tier crece sin gate — mitigado porque collect_local_env captura
  por prefijo (un knob nuevo entra a la metadata solo con existir); y la
  decisión puede revertirse por-knob sin romper nada (env sigue leyéndose).
Estado nuevo: promoted (la decisión), experimental (nada — no hay código
  nuevo de comportamiento, solo procedencia).
```

## El razonamiento en una línea

braze es laboratorio antes que producto: lo que el laboratorio necesita de
un knob de despliegue no es poder escribirlo en un archivo compartible —es
que **ningún sweep pueda volver a correr con configuración invisible**. Eso
lo da `metadata.local_env`, no `config.json`.
