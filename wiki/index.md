---
type: wiki-index
created: 2026-07-14
---

# Wiki de braze

Árbol de conocimiento de este proyecto: arquitectura, decisiones, gotchas,
patrones. Complementa `CLAUDE.md` (estado táctico actual, cambia seguido) con
lo que no cabe ahí — el porqué de las decisiones y el contexto que un
colaborador nuevo (humano o IA, incluyendo un modelo local operando solo)
necesita para no repetir preguntas ya respondidas.

**Estado táctico actual**: ver `../CLAUDE.md`

## Cómo se usa (resumen)

- `/wiki log <op> "<título>" [detalle]` — registra un evento en `log.md` (bajo costo, hacelo seguido)
- `/wiki grow` — revisa el log pendiente y propone páginas nuevas o actualizadas (con tu confirmación)
- `/wiki find <tema>` — busca en el wiki
- `/wiki status` / `/wiki lint` — salud del árbol

Ops sugeridas para `log`: `decision` · `bug` · `patron` · `hito` · `infra` · `nota`

## Páginas

- [[venue-y-review-emse]] — venue del paper (EMSE) y veredicto del review (Major Revision, 5 issues)
- [[hallazgo-composicion-basta]] — gemma4:e4b solo ≈ compuesto braze ≈ loop bare, el hallazgo empírico central de la resolución del review
- [[grader-validation]] — BRAZE_BENCH_KEEP_SESSIONS + validación 62/62 del grader automático
- [[modelos-locales-thinking]] — thinking vs no-thinking en Ollama, gemma4:e4b vs gpt-oss:20b, el crash U-21/U-22

## Documentación existente (`docs/`, pre-wiki)

42 documentos sueltos en `docs/`, sin índice previo — listados acá
agrupados por patrón de nombre, sin mover los archivos originales.
Candidatos naturales para que `/wiki grow` los destile en páginas
propias más adelante.

### Auditorías

- [../docs/AUDITORIA-2026-07.md](../docs/AUDITORIA-2026-07.md)
- [../docs/AUDITORIA-2026-07-v2.md](../docs/AUDITORIA-2026-07-v2.md)
- [../docs/AUDITORIA-2026-07-v3.md](../docs/AUDITORIA-2026-07-v3.md)
- [../docs/AUDITORIA-2026-07-v4.md](../docs/AUDITORIA-2026-07-v4.md)
- [../docs/AUDITORIA-2026-07-v5.md](../docs/AUDITORIA-2026-07-v5.md)
- [../docs/AUDITORIA-2026-07-v6.md](../docs/AUDITORIA-2026-07-v6.md)
- [../docs/AUDITORIA-2026-07-v7.md](../docs/AUDITORIA-2026-07-v7.md)

### Design docs pre-registrados (A/B, criterios adopt/reject)

- [../docs/constrained-decoding-ab-design.md](../docs/constrained-decoding-ab-design.md)
- [../docs/explorador-aislado-ab-design.md](../docs/explorador-aislado-ab-design.md)
- [../docs/external-harness-baseline-design.md](../docs/external-harness-baseline-design.md)
- [../docs/gemma4-e4b-solo-baseline-design.md](../docs/gemma4-e4b-solo-baseline-design.md)
- [../docs/local-backend-stencil-design.md](../docs/local-backend-stencil-design.md)
- [../docs/project-memory-design.md](../docs/project-memory-design.md)

### Sweeps (resultados crudos + análisis)

- [../docs/sweep-capacity-hardware-2026-07-13.md](../docs/sweep-capacity-hardware-2026-07-13.md)
- [../docs/sweep-constrained-decoding-2026-07-12.md](../docs/sweep-constrained-decoding-2026-07-12.md)
- [../docs/sweep-curva-multiescala-2026-07-10.md](../docs/sweep-curva-multiescala-2026-07-10.md)
- [../docs/sweep-gemma-diagnostic-minimal-1rep-2026-07-11.md](../docs/sweep-gemma-diagnostic-minimal-1rep-2026-07-11.md)
- [../docs/sweep-lead-3brazos-2026-07-10.md](../docs/sweep-lead-3brazos-2026-07-10.md)
- [../docs/sweep-matriz-4brazos-2026-07-10.md](../docs/sweep-matriz-4brazos-2026-07-10.md)
- [../docs/sweep-planlead-2026-07-11.md](../docs/sweep-planlead-2026-07-11.md)
- [../docs/sweep-planlead-taskslead-postfix-2026-07-11.md](../docs/sweep-planlead-taskslead-postfix-2026-07-11.md)
- [../docs/sweep-planner-ab-2026-07-11.md](../docs/sweep-planner-ab-2026-07-11.md)
- [../docs/sweep-search-tools-ab-2026-07-11.md](../docs/sweep-search-tools-ab-2026-07-11.md)
- [../docs/sweep-search-tools-ab-n15-2026-07-12.md](../docs/sweep-search-tools-ab-n15-2026-07-12.md)
- [../docs/sweep-search-tools-ab-postgate-2026-07-12.md](../docs/sweep-search-tools-ab-postgate-2026-07-12.md)
- [../docs/sweep-si2-lead-ab-2026-07-09.md](../docs/sweep-si2-lead-ab-2026-07-09.md)

### Usability logs (sesiones reales contra el binario)

- [../docs/usability-log-2026-07-07.md](../docs/usability-log-2026-07-07.md)
- [../docs/usability-log-2026-07-07-si1.md](../docs/usability-log-2026-07-07-si1.md)
- [../docs/usability-log-2026-07-07-si2.md](../docs/usability-log-2026-07-07-si2.md)
- [../docs/usability-log-gptoss20b-playground-2026-07-13.md](../docs/usability-log-gptoss20b-playground-2026-07-13.md)
- [../docs/usability-log-template.md](../docs/usability-log-template.md)

### Review EMSE 2026-07-13

- [../docs/emse-review-2026-07-13-checklist.md](../docs/emse-review-2026-07-13-checklist.md)
- [../docs/grader-validation-2026-07-13.md](../docs/grader-validation-2026-07-13.md)
- [../docs/power-increase-2026-07-13.md](../docs/power-increase-2026-07-13.md)

### Sueltos

- [../docs/H-1-cierre-cache-tokens.md](../docs/H-1-cierre-cache-tokens.md)
- [../docs/harness-engineering-hooks-skills-2026-07-10.md](../docs/harness-engineering-hooks-skills-2026-07-10.md)
- [../docs/ollama-gemma-adaptation-2026-07-11.md](../docs/ollama-gemma-adaptation-2026-07-11.md)
- [../docs/opencode-a-braze.md](../docs/opencode-a-braze.md)
- [../docs/self-improvement-exercises.md](../docs/self-improvement-exercises.md)
- [../docs/SOTA-2026-07.md](../docs/SOTA-2026-07.md)
- [../docs/TUI-INVESTIGACION-2026-07.md](../docs/TUI-INVESTIGACION-2026-07.md)
- [../docs/verify-refs-2026-07-13.md](../docs/verify-refs-2026-07-13.md)


