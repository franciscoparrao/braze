# Registro de prueba de usabilidad — braze

Copia esta plantilla a un archivo nuevo por sesión de prueba (p.ej.
`docs/usability-log-2026-07-07.md`) y complétala mientras usas `braze`
como usuario, no como desarrollador. Ver `docs/usability-testing-guide.html`
para el protocolo completo de escenarios.

**Fecha**:
**Backend/modelo probado**: (p.ej. `ollama:qwen3.5-coder` en Nitro, `anthropic:claude-...`)
**Commit de braze**: (`git rev-parse HEAD`)
**Modo**: TUI / texto plano

## Registro

| # | Escenario | Qué esperaba | Qué pasó | Severidad | Sesión (`tools/braze_sessions.py show <id>`) |
|---|-----------|--------------|----------|-----------|-----------------------------------------------|
| 1 | | | | Bloqueante / Molesto / Menor | |
| 2 | | | | | |
| 3 | | | | | |

**Severidad** — guía rápida:
- **Bloqueante**: no se puede completar la tarea, o el resultado es incorrecto sin que el usuario lo note.
- **Molesto**: se completa, pero con rodeos, mensajes confusos, o pasos de más.
- **Menor**: cosmético — formato, wording, un detalle de la TUI.

## Notas generales

(Impresiones sueltas que no encajan en una fila de la tabla — p.ej. "el
prompt del sistema no menciona X", "la latencia de Nitro se sintió Y".)

## Hallazgos que ameritan seguimiento

(Si algo de lo anotado arriba parece un hallazgo real — no solo una
preferencia — dale un id corto (p.ej. `U-1`) y una línea de una frase,
mismo estilo que los hallazgos `N-xx`/`F-xx`/`E-xx` de
`docs/AUDITORIA-2026-07*.md`, para que sea fácil promoverlo a un ítem de
PLAN.md si se decide actuar sobre él.)
