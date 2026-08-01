---
type: wiki-page
created: 2026-07-14
tags: [paper, emse, review]
---

# Venue del paper y review EMSE

## Qué es

El paper de `braze` (`paper/main.tex`) tiene como venue objetivo
**Empirical Software Engineering (EMSE, Springer)**, decidido
2026-07-12. El manuscrito pasó por `/paper-review-emse` (protocolo
completo: 3 personas independientes + comparación contra 3 peers reales
del corpus EMSE + auditoría visual de las 3 figuras) el 2026-07-13, con
veredicto **Major Revision**.

## Por qué existe

EMSE se eligió sobre TMLR/JAIR por requisito de Impact Factor JCR. Como
el vault de revisión (`~/vault/journals/`) solo tenía journals de
geociencias/GIS (dominio previo del usuario), hubo que dar de alta EMSE
desde cero: `outline.md` (Aims & Scope oficial), `recent.bib` (1000
papers de WoS, validado 97.4% con abstract / 100% con DOI),
`editors.md` (editorial board completo, 130 miembros — EIC Robert Feldt
+ Thomas Zimmermann), `notes.md` (JCR 2025: JIF 3.4, Q2), style
profile, y el skill `/paper-review-emse`.

## Detalles

### Veredicto del review (2026-07-13)

**Major Revision** — el aparato empírico (pre-registro, Wilson/Newcombe
CIs, disclosure de threats to validity) se calificó como sólido; los
issues son huecos de diseño concretos, no problemas estructurales.
Checklist completo: `docs/emse-review-2026-07-13-checklist.md`.

5 issues críticos identificados:

1. **Sin baseline de harness externo** — toda comparación era `braze`
   contra sí mismo. Resuelto → [[hallazgo-composicion-basta]].
2. **Sin baseline solo del lead model** (`gemma4:e4b`) — el headline
   "1B+lead supera a 3B/7B" nunca se comparó contra lo que el propio
   lead saca solo. Resuelto → [[hallazgo-composicion-basta]].
3. **Pre-registro auto-alojado en git** — evidencia más débil que un
   registro externo (OSF), especialmente relevante porque EMSE tiene su
   propio Open Science Review Board. Parcialmente resuelto: se empezó a
   usar OSF para los criterios nuevos, pero el registro efectivo quedó
   pendiente (sin credenciales en el entorno de ejecución).
4. **Sin validación independiente del grader automático**. Resuelto →
   [[grader-validation]].
5. **Manuscrito incompleto** — título, afiliación, DOIs Zenodo, commit
   hash faltante, apéndice de pre-registro sin transcribir. Pendiente
   (Fase 5 del plan de resolución).

### Peers usados en la comparación

1. "An empirical study of testing practices in open source AI agent
   frameworks and agentic applications" (2026) — DOI
   10.1007/s10664-026-10857-9
2. "Securing LLM-in-the-loop software..." (2026) — DOI
   10.1007/s10664-026-10820-8
3. "Which design decisions in AI-enabled mobile applications contribute
   to greener AI?" (2024) — DOI 10.1007/s10664-023-10407-7

## Relacionado

- [[hallazgo-composicion-basta]] — la resolución de los issues 1 y 2
- [[grader-validation]] — la resolución del issue 4

## Referencias

- `docs/emse-review-2026-07-13-checklist.md`
- `~/vault/journals/emse/reviews-generated/2026-07-13_16-34_braze-harness-paper.md` (review completa)
- `~/vault/journals/emse/` (outline, recent.bib, editors.md, notes.md)
- `~/.claude/skills/paper-review-emse/SKILL.md`
