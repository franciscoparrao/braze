# Nota de lectura: Apache Maka y Cordis — el campo avanza en producto y en formalismo, no en medición

Fecha: 2026-08-27. Dos trabajos que **no compiten con el framing del
Paper 1** pero que extienden el argumento del Paper 3 por un flanco
que la escala de cuatro niveles no cubría: hasta ahora esa escala
clasificaba *papers que evalúan harnesses*. Estos dos no son eso —
uno es infraestructura de evaluación en producción y el otro un
formalismo de arquitectura — y ninguno mide.

## Apache Maka (incubating)

`github.com/apache/maka`. Workspace de agentes local-first en
TypeScript/Electron/React con SQLite. Repo público desde
**2026-05-27**, en incubación ASF desde **2026-08-13**, v0.11 el
18-ago (375 PRs, 24 contribuidores). Iniciado por Jie Wen
(jackwener), PMC de Arrow/DataFusion/Doris. Primitiva central: un
event log append-only donde entran mensajes del modelo, tool calls,
tool results, **decisiones de permiso** y eventos de terminación.

**Solapamiento arquitectónico con braze**: alto y por convergencia,
no por préstamo. Runtime Host → SessionManager → AgentRun → Runtime
Event Log → proyecciones (context/session/UI) es el mismo diseño
"log inmutable + vistas derivadas" que acá resuelve la compactación
diferencial. La diferencia de fondo: Maka hace del log la fuente de
verdad y deriva todo; braze mantiene estado durable separado de la
ventana táctica.

**Lo que importa acá es su eval**, `maka eval run <spec> --out <dir>`,
casi isomorfo a `braze-bench`:

| Maka | braze-bench |
|---|---|
| `Experiment = benchmark + executor + subjects + tasks + repetitions` | brazos × repeticiones × backends |
| `Cell = task × repetition × subject` | la misma celda |
| `infra retry` distinguido de `continuation` | `HarnessError` fuera del denominador |
| kernel de resultados: score, uso normalizado, **costo atribuible**, duración, razón de fallo | idem |

**Y algo que ellos tienen y braze no**: *"the earliest valid attempt
is authoritative; operators cannot choose a preferred outcome"* —
anti-cherry-picking **codificado en la herramienta**, no confiado a
la disciplina del operador. Vale robarlo; es exactamente la clase de
garantía que este proyecto defiende y deja en manos del método.

**Lo que no tienen**: en README ni ARCHITECTURE.md aparece nada sobre
semillas, control de varianza, no-determinismo o potencia
estadística. Las repeticiones se definen como *"a new experimental
sample"* y ahí termina. *Caveat de honestidad: leí los dos documentos
de nivel superior, no `packages/eval`. El veredicto firme exige leer
ese paquete — pendiente antes de citarlo en el Paper 3.*

## Cordis / "A Programming Paradigm for Spatiotemporal Composability"

Shi, Zhang & Cui (Peking University + DeepSeek-AI), 92 pp., fechado
**2026-08-26**, sin venue. Copia en `docs/paper.pdf`.

Formaliza composición dinámica en dos dimensiones ortogonales:
**efectos revertibles** (cada transformación de contexto lleva su
inversa, que el runtime retiene, de modo que remover un componente
deshace exactamente lo que hizo) y **coeffects reactivos** (cada
componente declara sus dependencias y cada cambio de contexto se
clasifica como activante, desactivante o neutro). Los unifica en el
*context paradigm*. Implementación en Cordis (TypeScript), case study
sobre Koishi y sus 4.000+ plugins.

**Por qué nos toca**: § 1.2.2 usa *self-evolving agent harnesses*
como una de sus dos motivaciones centrales, y la lista de capacidades
que enumera es la de braze — componer suites de tools, gobernar
permisos y sandboxing, estado de sesión y persistencia, gestión de
contexto y memoria, orquestar subagentes. Conecta con la línea
meta-harness que el Paper 2 ya cita (`luo2026autodesign`), pero desde
el lado formal.

**Por qué no es accionable todavía**: braze es composición estática —
los tres traits congelados se resuelven en compile time. Lo dinámico
acá son *datos* (skills, servidores MCP), no *código*. El problema
que resuelven aparecería con la Fase 2 diferida (skills cargables,
hooks plugueables). Dos detalles guardados para ese día: § 6.4 nombra
Rust explícitamente (traits para extender el context type desde el
módulo del proveedor; macros procedurales para emitir la declaración
tipada junto con el accessor, evitando una primitiva de interceptación
genérica), y § 6.3 sostiene que el sandbox real exige un mecanismo
fuera del lenguaje — el argumento del Landlock del Paquete 4.

**Nota de diseño con valor propio**: el rebuild de backend del
`/model` picker y J-12 (rehidratar skills del rollout log tras ese
rebuild) son un problema de composabilidad temporal resuelto **ad
hoc**, que es literalmente lo que critican. En su vocabulario, el
rebuild es un efecto que debería llevar su inversa en vez de que cada
sitio recuerde restaurarse.

## El patrón, formulado con precisión

La tentación es decir "nadie mide" y meter a los cinco en el mismo
saco. Sería el mismo error que ya se corrigió con Belief Divergence
el 25-ago. La formulación honesta distingue **tres poblaciones
distintas**, y el punto es que el hueco cruza a las tres:

1. **Papers que evalúan harnesses** — cubiertos por la escala de
   cuatro niveles de `nota-lectura-harnessbench-belief-2026-08-25.md`.
   El hueco va de "sin control ni inferencia" (AutoDesign,
   Meta-Harness) a "control de diseño sin inferencia" (Belief
   Divergence, Harness-Bench).
2. **Infraestructura de evaluación en producción** — Apache Maka. Su
   procedimiento es en algunos aspectos **más** estricto que el de
   varios papers (el anti-cherry-picking está codificado), y aun así
   la varianza no aparece en su modelo. Un harness de eval respaldado
   por la ASF, que muchos van a usar, cuyas repeticiones no alimentan
   ninguna decisión estadística.
3. **Formalismos de arquitectura de harness** — Cordis. No evalúa
   nada, y su propio *Threats to validity* lo dice: evidencia
   *"observational rather than a controlled comparison against an
   alternative architecture"*, un resultado *"de existencia y
   adopción, no cuantitativo"*, con la medición del overhead contra
   un baseline declarada como trabajo futuro. Noventa y dos páginas
   sobre arquitectura de harnesses sin medir un harness.

**La frase para el Paper 3**: el campo está madurando por sus dos
extremos —producto e infraestructura por un lado, formalismo por el
otro— y la capa del medio, decidir con evidencia cuál diseño es
mejor y bajo qué criterio, es la que sigue vacía. No es que la gente
mida mal; es que medir todavía no es parte de lo que se espera de un
trabajo de harness.

Eso es más fuerte y más justo que "no controlan el ruido", y no
depende de caracterizar mal a nadie.

## Qué NO hacer con esto

- **No van al Related Work del Paper 1.** Ese párrafo es sobre los
  trabajos que comparten su framing (Harness-Bench, Belief
  Divergence) — ver la sección redactada en la nota del 25-ago.
  Maka y Cordis no compiten con la tesis harness-vs-escala; meterlos
  ahí diluiría el párrafo.
- **No citar Cordis sin verificarlo.** Preprint de un lab de IA, sin
  venue, fechado el día que se leyó. Mismo criterio que se aplicó a
  Maka.
- **No citar el eval de Maka como "sin control de varianza" sin leer
  `packages/eval`.** El claim del Paper 3 no puede descansar en dos
  documentos de nivel superior — es exactamente la lección del
  25-ago (no caracterizar el rigor de un trabajo por su abstract).

## Riesgo para el Paper 1, evaluado

Maka es público desde el 27-may, **dos meses antes** del freeze del
29-jul. No lo minimizo, pero no es el caso de Harness-Bench o Belief
Divergence: eso era literatura arbitrada; esto era, al 29-jul, un
repo de GitHub sin publicación. EMSE evalúa contra literatura. El
riesgo real no es la omisión sino que un revisor que lo conozca
pregunte cómo se relaciona braze con él — y la respuesta está en esta
nota: convergencia arquitectónica independiente, con braze aportando
el `LocalBackend` in-process que Maka no tiene (Maka es API-bound: sin
acceso al sampler no hay stencil ni ablaciones por token).

## Acciones

1. Leer `packages/eval` de Maka antes de citarlo en el Paper 3.
2. Verificar Cordis (venue, si aparece en arXiv) antes de citarlo.
3. Considerar adoptar el *earliest-valid-attempt* de Maka como
   política explícita del bench — hoy es disciplina, no mecanismo.
4. Al outline del Paper 3: la formulación de tres poblaciones de
   arriba, que amplía la escala de cuatro niveles en vez de
   reemplazarla.

---

## CORRECCIÓN (2026-08-28): leí `packages/eval` y me equivoqué con Maka

La acción 1 de esta nota decía "leer `packages/eval` de Maka antes de
citarlo en el Paper 3". Hecho, clonando el repo. **La caracterización de
arriba es falsa y hay que retirarla.**

Lo que decía esta nota: *"en README ni ARCHITECTURE.md aparece nada
sobre semillas, control de varianza, no-determinismo o potencia
estadística"*. Literalmente cierto para esos dos archivos, y
completamente engañoso sobre el proyecto: el rigor no está en el código
del paquete sino en **`docs/eval/`, donde Maka publica sus propios
reportes de evaluación**.

### Lo que esos reportes hacen

De `terminal-bench-2.1-deepseek-v4-flash-four-arm.md`:

- **McNemar exacto de dos colas** sobre pares discordantes, con las 89
  tareas como unidades pareadas. Reportan p en todas las comparaciones,
  incluidas las no significativas (Codex−Maka, p = 0,359).
- **Corrección de multiplicidad**, declarada en Limitations: Bonferroni
  sobre las seis comparaciones mueve el umbral a 0,0083 y verifican que
  sus dos resultados lo sobreviven.
- **Un piso de ruido medido, y lo llaman "the most useful number in
  this report"**: dos corridas del mismo brazo con configuración
  idéntica dieron **19,10 % de tareas que cambian de resultado** con el
  score neto moviéndose 3. *"Roughly a fifth of this suite is decided by
  run-to-run variation rather than by any property of the harness under
  test."*
- **Derivan el MDE de ese piso**: *"a single run cannot validate a
  change worth fewer than roughly ten tasks"*.
- **Separan confound de varianza** para los brazos cuya configuración sí
  cambió entre corridas, en vez de leer su delta como efecto.
- **Declaran una asimetría de harness no controlada** —Maka lleva un
  system prompt externo y una política de prune que los otros tres
  brazos no tienen— y admiten que la corrida *"has no prune-off or
  prompt-off control"*.
- **Reportan una discrepancia de telemetría sin resolver** y se abstienen
  del claim que dependería de ella.
- Hashes SHA-256 de la evidencia congelada.

### Consecuencias

1. **Maka sale de la "población 2".** No es infraestructura que omite la
   varianza: es infraestructura que la mide, la usa para derivar un MDE
   y publica sus limitaciones. Con eso, la población 2 se queda **sin
   ejemplo**, y la formulación de tres poblaciones no se sostiene tal
   como está escrita arriba.
2. **Su 19 % corrobora nuestro 25 %.** El A/A de MXFP4 dio 25,0 % de
   celdas discordantes; Maka mide 19,1 % de tareas que voltean, con otro
   harness, otro benchmark, otro modelo y otro dominio. Dos mediciones
   independientes en el mismo orden de magnitud. Eso **refuerza** el
   Paper 3 aunque le quite un ejemplo del hueco: el argumento deja de
   ser "nadie mide" y pasa a "cuando se mide aparece ~20-25 %, y los
   umbrales publicados no lo respetan".
3. El *earliest-valid-attempt* sigue valiendo como mecanismo a robar.

### La lección, por tercera vez en tres días

Belief Divergence (25-ago), HarnessOpt-Bench (28-ago) y ahora Maka: en
los tres casos caracterizar por el documento de nivel superior produjo
una afirmación injusta, y en los tres la versión corregida resultó **más
difícil de refutar** que la original.

Peor acá: esta nota **ya declaraba** el caveat ("leí los dos documentos
de nivel superior, no `packages/eval`") y aun así la caracterización
entró al outline v2 del Paper 3 como si estuviera establecida. Escribir
el caveat no basta si después se cita el claim sin él.

Regla operativa: **ningún trabajo entra a un argumento del Paper 3 sin
que alguien haya leído su evidencia primaria.** Si el caveat existe, el
claim no se usa hasta levantarlo.
