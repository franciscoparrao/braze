# Nota de lectura: *Coding Benchmarks Are Misaligned* (2606.17799) y *SkillsBench* (2602.12670)

Fecha: 2026-08-29. Cierra las tres lecturas pendientes de
`nota-campo-harness-2026-08-25.md`.

---

## 1. *Position: Coding Benchmarks Are Misaligned with Agentic SE*

Gorinova, Baker, Heineike, Shaposhnikov, Willoughby, Knox (**Tessl**,
Londres), 18-jul-2026, 6 págs. Posición, con experiencia de haber
construido y operado NS2, su propio *system harness*.

**Tesis**: los benchmarks actuales colapsan modelo, harness, entorno y
contexto en un único score end-to-end contra una solución de referencia,
sin señal a nivel de componente. Tres síntomas: confunden modelo con
harness, anclan en una referencia única, y no dan señal por componente.

### Lo que aporta al proyecto

**Su Tabla 1 es material citable directo.** Terminal-Bench con el modelo
**fijo** (Claude Opus 4.6) a través de nueve harnesses:

| harness | accuracy |
|---|---|
| ForgeCode | 79,8 ± 1,6 |
| Capy | 75,3 ± 2,4 |
| Terminus-KIRA | 74,7 ± 2,3 |
| … | … |
| Terminus 2 | 62,9 ± 2,7 |
| **Claude Code** | **58,0 ± 2,9** |

Más de **20 puntos** con el modelo constante, y Claude Code último. Es
la tesis del Paper 1 en una tabla ajena, con barras de error.

**Y dos referencias industriales del término residual** —justo lo que el
outline del Paper 3 necesita para el threat de circularidad—:

- **AI21**, sobre más de 200.000 corridas de SWE-bench: *"orchestration
  choices, container allocations, and evaluation seeds materially move
  the pass rate at fixed model and fixed harness"* (ref [1]).
- **Anthropic**, *"Quantifying infrastructure noise in agentic coding
  evals"* (ref [39]).

Dos organizaciones grandes documentando que el score se mueve **con
modelo y harness fijos**. Es exactamente el `σ²_ε` que la nota sobre
Binding Constraint identificó como el término faltante, y viene de
quienes tienen el volumen para medirlo. Hay que verificarlas antes de
citarlas: son posts, no papers.

**Su remedio sugerido describe lo que este proyecto ya hace.** § 4.1:
*"submissions should require relevant metadata: what model, agent
harness version, environment hash, and dataset version… and include at
least one ablation across a non-model axis against a fixed baseline"*.
Eso es `RunMetadata` + `engine_version` + las claves `+ablate:`. No es
validación de nadie, pero conviene saber que la práctica que acá se
construyó por necesidad es la que un paper de posición pide.

**Su § 4.3** argumenta que sin señal por componente el ciclo de mejora
degrada a ablación guiada por intuición, y que *"if the harness is a
composition of components, we should aim to evaluate components
separately"*. Es el argumento de las palancas ablacionables.

---

## 2. *SkillsBench: Benchmarking How Well Agent Skills Work Across Diverse Tasks*

Li, Liu, Chen, You et al. (60+ autores, ~30 instituciones), 14-jun-2026,
42 págs. 87 tareas en 8 dominios, 18 configuraciones modelo×harness,
9.396 trayectorias.

**Resultado central**: skills curadas suben el pass rate task-macro de
**33,9 % a 50,5 %** (+16,6 pp; 25,5 % de ganancia normalizada), con
heterogeneidad grande por configuración: **+4,1 a +25,7 pp**.

### Su rigor, que es alto

- **Diseño pareado por construcción**: cada condición corre la misma
  tarea en el mismo contenedor, y los deltas son diferencias pareadas a
  nivel (configuración, tarea), no diferencias entre pools.
- **Tres trials por celda** contra un marco fijo de 87 × 3.
- **IC 95 %** en la figura principal, para baseline y total.
- **Filtrado anti-fuga**: rechazan skills que nombren archivos, comandos
  o salidas específicas de la tarea; las instrucciones nunca nombran qué
  skill usar. Sin eso *"a Skill becomes a hidden answer key"*.
- Rechazan tareas *"with no measurable separation between conditions"*
  como bajo-señal.

### Los dos hallazgos que tocan a braze

**Finding 1**: *"Skill efficacy is an empirical property of a specific
agent stack rather than a universal constant."* Es la tesis del proyecto
—el harness como variable— trasladada a skills, y medida sobre 18
stacks.

**Finding 2**: los sistemas más fuertes no son los que más ganan.
OpenHands + Gemini 3.5 Flash parte de 41,1 % y gana solo +7,1 pp;
OpenHands + GLM 5.1 gana +25,7 pp. *"High base capability does not imply
high Skill leverage."*

Eso es directamente relevante para la palanca **call-time skills**, hoy
OFF por falta de contenido
(`docs/precondicion-call-time-skills-2026-08-29.md`): dice que la
ganancia esperable depende del stack y no se hereda de su +16,6 pp.
Medirla acá sigue siendo necesario.

### Un contraste de diseño con D′

Usan el formato Agent Skills de Anthropic (`SKILL.md` + recursos), igual
que `braze-skills`. Pero sus agentes **descubren y activan** las skills
por *progressive disclosure*; braze es **explicit-only** (`$skill`), y
la invocación call-time recién implementada es un tercer camino: la
decide el harness en un evento de ejecución.

Su tercera condición —**skills auto-generadas** por el propio agente
antes de resolver— es un brazo que braze no tiene y que sería barato
como comparación futura.

**Su diseño de tres condiciones (sin / curada / auto-generada) con pareo
por (configuración, tarea) es el modelo a seguir** para el pre-registro
de call-time skills, y refuerza la condición ya escrita de usar al menos
dos skills: con una sola no se separa palanca de contenido, y ellos
tienen 87 tareas para promediar esa varianza.

---

## Consecuencias, en orden

1. **Paper 3**: verificar y citar AI21 [1] y Anthropic [39] como
   evidencia industrial del término residual. Atacan el threat de
   circularidad mejor que cualquier medición propia, porque vienen de
   volúmenes que este proyecto no puede alcanzar.
2. **Paper 1** (eventual revisión): la Tabla 1 de Tessl es la tesis del
   paper en datos ajenos, con barras de error.
3. **Pre-registro de call-time skills**: adoptar el diseño de tres
   condiciones y el pareo por (configuración, tarea) de SkillsBench.
4. **Bib**: `gorinova2026misaligned`, `li2026skillsbench`, con
   `/verify-refs` como las otras nueve.
5. Con esto quedan **cerradas las tres lecturas pendientes** del mapeo
   del campo del 25-ago.
