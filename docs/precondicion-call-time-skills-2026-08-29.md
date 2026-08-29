# Precondición de call-time skills: no se cumple, y por qué eso es distinto al gate

Fecha: 2026-08-29
Palanca: `+ablate:call-time-skills` (Recuris § 2.2.2), implementada en
`1db3d70`, OFF por default.

## El chequeo

La doc de la clave de ablación ya advertía el modo de falla: *"la fila
corre IDÉNTICA al control si el registro de skills no tiene ninguna con
`tools:` en su frontmatter. Es un nulo silencioso — el sweep saldría
limpio y sin efecto por construcción, no por medición."*

Medido hoy:

| | |
|---|---|
| `SKILL.md` en el repo de braze | **0** |
| Skills del usuario con frontmatter `tools:` | **0** |
| Paths de skills en la config | ninguno |

**La precondición no se cumple.** Un A/B lanzado hoy mediría cero por
construcción.

## Pero esto NO es el caso del gate de evidencia

La diferencia importa y conviene no confundirlas:

- El **gate de evidencia** resultó no medible por una razón que no
  depende de nosotros: el modelo que comete el error no usa la lista, y
  el que la usa no comete el error
  (`hypothesis-2026-08-28-task-evidence-gate.md`). Para reabrirlo haría
  falta encontrar un executor con ambas propiedades, y no hay candidato.
- **Call-time skills** tiene la precondición trivialmente satisfacible:
  basta escribir una skill con `tools:`. No depende del comportamiento
  del modelo.

El A/B es viable. Lo que falta no es instrumento sino **contenido**.

## Y ese contenido ES el tratamiento

Acá está el problema de diseño, y por eso esto es una nota y no un
sweep: si escribo una skill y luego mido si ayuda, **la skill es el
tratamiento**, y su calidad determina el resultado. Una skill mala daría
un nulo que se leería como "la invocación call-time no sirve" cuando
mediría "esta skill no sirve".

Es exactamente el error que el brazo U1 de Q0 evitó fijando la redacción
antes de correr (`d51951a`), y el mismo que el pre-registro del QLoRA
declara sobre el fingerprint del experto.

Consecuencias para un pre-registro futuro de esta palanca:

1. **El contenido de la skill se fija y se commitea antes de correr**,
   igual que la redacción de U1.
2. **Necesita más de una skill**, o el resultado no separa "la palanca
   funciona" de "esta skill funciona". Mínimo dos, sobre herramientas
   distintas.
3. El candidato natural para la primera: una skill de `edit_file` que
   codifique los modos de falla ya medidos por el proyecto — el hallazgo
   del 2026-07-28 sobre caracteres que el modelo entiende y no puede
   emitir, y la convención de reportar la primera divergencia con
   codepoint. Es guía que el proyecto sabe que sirve, lo que reduce el
   riesgo de confundir palanca con contenido.
4. **Declarar de antemano el conteo de intercepciones**, igual que el
   criterio 3 del gate: un nulo con cero intercepciones no distingue "no
   ayudó" de "no disparó". El evento `SkillLoaded` con
   `trigger: "call_time"` ya lo hace contable sin instrumentar nada.

## Estado

La palanca queda **implementada y OFF**, como `task-evidence`, pero por
una razón distinta y reversible: falta escribir el contenido, no falta
un executor. No se pre-registra todavía porque el pre-registro tendría
que fijar unas skills que aún no existen.
