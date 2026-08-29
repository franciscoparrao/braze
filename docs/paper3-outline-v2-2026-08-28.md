# Outline v2 del Paper 3 — reencuadre tras Parupudi y Recuris

Fecha: 2026-08-28
Estado: **reemplaza el scoping de
`docs/paper3-outline-gates-2026-08-21.md`**, que sigue siendo válido en
su evidencia propia y **obsoleto en su premisa**.
Motivo: entre el 25 y el 26 de agosto aparecieron dos trabajos que
hacen, con rigor, lo que el outline v1 decía que nadie hacía.

## Lo que cambió, y por qué obliga a reescribir la premisa

El v1 abría con *"cuatro sistemas independientes… y ninguno resuelve
cómo decidir bajo ruido"*. Esa frase ya se había erosionado dos veces
antes de esta nota —HarnessOpt-Bench con su held-out inaccesible
(`paper3-experimento-central`), y Belief Divergence con su grilla de
semillas apareadas (`nota-lectura-harnessbench-belief`)— y el 25-26 de
agosto se rompió del todo:

- **Parupudi, *There Is No Neutral Harness*** (arXiv:2608.21382).
  Bootstrap pareado sobre ítems, decoding determinista de semilla
  única, per-item records liberados, y una sección de Limitations que
  desarma sus propios claims uno por uno.
- **Recuris** (arXiv:2608.24876). Bootstrap task-clustered de 10.000
  remuestras, McNemar exacto para contrastes binarios, y —lo decisivo—
  **un piso de ruido medido y usado como criterio**: re-corrieron un
  paquete sin cambios tres días después y obtuvieron un intervalo de
  [−6,98, +7,27], del que concluyen que *"treat differences of a few
  points as within run-to-run variation regardless of their interval"*.
  Su Apéndice B además ablaciona su propia capa de ingeniería y reporta
  que no aporta nada medible.

**Un paper que sostenga "el campo no mide" queda refutado por dos
contraejemplos de la misma semana.** Escribirlo así sería el error que
este proyecto persigue en otros.

## La premisa que SÍ se sostiene

No es sobre el campo, es sobre **los gates**. Reformulada:

> Los sistemas que optimizan el harness automáticamente deciden qué
> commitear con un umbral sobre estimaciones puntuales, y **ninguno
> deriva ese umbral del ruido de su propia configuración**. Este trabajo
> mide cuánto cuesta esa omisión, y muestra que el umbral —no la
> presencia de estadística— es la variable que hace el trabajo.

Tres distinciones que hay que mantener separadas y que el v1 mezclaba:

1. **Sobreajuste de búsqueda** — elegir el candidato que memorizó el
   train. Lo ataca un held-out inaccesible (HarnessOpt-Bench, Scale AI:
   la partición de test es inaccesible durante toda la búsqueda y un
   entorno de ejecución de confianza hace cumplir el límite, mide el
   consumo del agente objetivo y versiona cada candidato para
   auditoría). **Verificado contra el texto completo el 2026-08-28**,
   no contra el abstract.

   Matiz que la lectura completa obliga a agregar: ellos **nombran** el
   problema del ruido —el optimizador debe *"separate real improvement
   from noise"*— pero como parte de la **capacidad que miden en el
   optimizador**, no como propiedad de su propia medición. La
   distinción de este outline sigue en pie: su held-out se evalúa una
   vez, y con evaluación estocástica ese score final tiene varianza que
   el protocolo no acota. Decir que "ignoran el ruido" sería falso;
   decir que no lo propagan a su métrica de reporte, no.
2. **Ruido de medición en el test final** — una ganancia en el held-out
   puede ser ruido igual. Lo ataca un piso medido (Recuris, en un
   dominio).
3. **Calibración del umbral contra ese ruido** — que el δ del gate se
   derive del piso en vez de elegirse. **Sigue sin hacerlo nadie**, y
   es lo que el experimento central mide.

## El resultado central (ya medido, 25-ago)

`scripts/paper3_false_acceptance.py`, 1.602 comparaciones nulas
construidas de 140 grupos con réplicas de configuración idéntica:

| regla | tasa de aceptación falsa |
|---|---|
| Meta-Harness (mejor score) | **24,4 %** |
| AutoDesign (`J_train↑ ∧ J_dev no baja`) | 14,4 % |
| HCL (`Δ ≥ 1` + anchor) | 14,4 % |
| HCL (`Δ ≥ 3` + anchor) | **0,7 %** |
| este proyecto (McNemar exacto, α=0,05) | 0,0 % |

**El resultado NO es la lista de sistemas reprobados sino la curva
umbral vs tasa de aceptación falsa.** δ=1 → 14,4 %; δ=3 → 0,7 %. La
misma estructura de gate, con el umbral movido, cae veinte veces. Eso
es constructivo, citable, y no depende de caracterizar mal a nadie.

Caveat que va en el mismo párrafo que el 0,0 %: con pocos pares
discordantes el McNemar exacto **no puede** bajar de 0,0625, así que
nuestro gate tampoco detectaría efectos moderados en suites chicas. Es
el MDE que el Paper 2 ya declaraba.

## Evidencia propia de ruido, actualizada

Tres medidas independientes, todas de datos ya existentes:

1. **Piso A/A del weight-quant** (28-ago, `scripts/weight_quant_close.py`):
   MXFP4 contra sí mismo, 68 celdas pareadas → **17 discordantes
   (25,0 %)**, McNemar p = 0,33. Una de cada cuatro celdas voltea sin
   que nada cambie.
2. **Partición y banda por ítem** (28-ago,
   `docs/analisis-fragilidad-discriminacion-2026-08-28.md`): sobre 5
   réplicas exactas, **53 % de los ítems voltea**; accuracy robusta
   0,471 contra media reportada 0,782 y optimista 1,000. **El 40 % de
   lo acreditado no sobrevive a repetir la corrida.**
3. **El falso positivo atrapado** (Paper 2): +13 tareas, McNemar
   p=0,011, Holm 0,021, criterio pre-registrado cumplido — y el control
   de prompt byte-idéntico subió +9 solo.

La (2) es la que dialoga directamente con Parupudi: él mide *config-lucky*
0,85 entre configuraciones con decoding determinista; nosotros medimos
*run-lucky* 0,40 entre réplicas exactas, que es la fuente que él
**excluye por diseño** y en el dominio que declara no cubrir
(*"we do not test open-ended generation, code, or agentic tasks"*).

## Contribuciones, reordenadas

1. **La curva umbral vs tasa de aceptación falsa** sobre reglas
   publicadas, medida en 1.602 comparaciones nulas. Es el núcleo.
2. **Distribución empírica del ruido de un banco agéntico** desde el
   archivo del proyecto, con las tres medidas de arriba. No existe
   públicamente.
3. **El caso de estudio del falso positivo** que pasa pre-registro,
   McNemar y Holm y se disuelve contra un control de prompt idéntico.
4. **Un gate de referencia y su precio** en corridas, con el MDE
   declarado.
5. **Un método para derivar el umbral del piso**, que es lo que la
   curva de (1) implica y nadie ofrece.

Baja de rango la crítica al campo; sube el instrumento.

## Posicionamiento honesto frente a los dos nuevos

**Recuris es el primero que hace lo que este paper pide** —medir el
piso y usarlo como criterio— y hay que decirlo en Related Work sin
rodeos. Pero lo hace como caveat de un dominio, no como objeto de
estudio: no reporta la distribución, no la generaliza, y no la usa para
calibrar un umbral. El Paper 3 aporta el número que justifica la
práctica que Recuris adoptó por prudencia.

**Parupudi es el vecino más cercano** y cubre el eje ortogonal
(configuración, no repetición) en el dominio complementario (MCQ, no
agéntico). Su § 4.4 —discriminación correlaciona con fragilidad— **no
replica en nuestros datos**: ρ = +0,134, IC95% [−0,191, +0,428] con
n=34, un intervalo que contiene tanto el cero como su +0,28. No es
nulo, es indeterminado, y así debe reportarse.

Consecuencia de tono: el paper deja de denunciar una ausencia y pasa a
**documentar un umbral en movimiento**, con dos trabajos de agosto como
evidencia de hacia dónde. Es más defendible y envejece mejor.

## Las poblaciones — RETIRADA la formulación de tres (2026-08-28)

La versión anterior de esta sección ponía a Apache Maka como ejemplo de
"infraestructura de evaluación en producción donde la varianza no
aparece en el modelo". **Es falso.** Al leer su repo (`docs/eval/`, no
solo README/ARCHITECTURE) resulta que sus reportes usan McNemar exacto,
declaran Bonferroni, y **miden un piso de ruido que llaman "the most
useful number in this report"**: 19,10 % de tareas que cambian de
resultado entre dos corridas de configuración idéntica, del que derivan
que *"a single run cannot validate a change worth fewer than roughly ten
tasks"*.

Con eso, la población 2 se queda sin ejemplo y la formulación de tres
poblaciones no se sostiene. Se retira hasta tener evidencia primaria de
algún caso que la ocupe.

Lo que queda en pie es más simple y más defendible: **entre los trabajos
que sí miden el ruido, ninguno usa esa medición para calibrar el umbral
de un gate automático de aceptación.** Recuris mide el piso y decide con
él de forma cualitativa ("differences of a few points"); Maka lo mide y
deriva un MDE cualitativo ("fewer than roughly ten tasks"); ninguno
convierte el piso en el δ de una regla. Eso es lo que la curva del
experimento central aporta.

### Y un dato externo que corrobora el nuestro

El 19,1 % de Maka —otro harness, otro benchmark, otro modelo, otro
dominio— está en el mismo orden de magnitud que el **25,0 %** del A/A de
MXFP4 medido acá (`scripts/weight_quant_close.py`) y que el **53 %** de
ítems frágiles sobre 5 réplicas. Tres mediciones independientes del
ruido run-to-run de un banco agéntico, todas entre ~20 % y ~50 % según
la unidad. Es evidencia externa de que la magnitud no es un artefacto
del banco propio, que era el threat de circularidad de este paper.

## Threats, actualizados

- **Simulación de reglas ajenas** (el principal, sin cambios): se
  critica la *regla de decisión*, no el sistema. Sin excepción.
- **Un régimen**: 3-20B locales, suites Rust con oráculo `cargo check`,
  un nodo. La tasa de aceptación falsa depende del ruido y el ruido del
  régimen. Acotar a "régimen ruidoso" y ofrecer el método, no afirmar
  que sus resultados publicados son falsos.
- **Circularidad parcial**: nuestro banco es la fuente del ruido.
  Mitigar reportando por configuración, no un número único.
- **Deriva del nodo** dentro del archivo: parte del "ruido" es deriva.
  Reportar separado donde el diseño lo permita.
- **NUEVO — procedencia de motor ausente en los sweeps históricos**:
  `engine_version` es del 27-ago; los sweeps que alimentan la
  distribución de ruido son anteriores y no registran con qué motor
  corrieron. El KV cache mueve el 82 % de los ítems (spread medio
  0,461), así que parte de la varianza atribuida a "ruido" podría ser
  configuración no registrada. Es un threat real y hay que declararlo.

## Qué falta para escribir

- [x] Claves de bib — HECHO 2026-08-28:
      `docs/paper3-refs-verificadas-2026-08-28.bib`, seis entradas con
      /verify-refs niveles 1 y 2 pasados (6/6 encontradas, cero
      retractadas, seis DOI agregados). Un hallazgo: `ren2025dynamics`
      está publicado en ICLR 2025 y OpenAlex no lo sabe — se cita como
      @inproceedings. Falta `ursekar2026harnessopt` (HarnessOpt-Bench):
      no hay PDF en disco, así que no se pudo verificar con fuente
      primaria.
- [x] `ursekar2026harnessopt` — HECHO 2026-08-28: PDF bajado,
      metadatos por `pdfinfo`, verificado nivel 1 y 2. Son 7 entradas.
- [ ] Leer `packages/eval` de Maka; verificar Cordis.
- [ ] Decidir si se versionan los JSON que alimentan los análisis (hoy
      viven en Nitro; sin ellos nada es reproducible por terceros).
- [ ] Venue con `/paper-match` cuando haya manuscrito. El v1 anotaba
      EMSE / ICSE-FSE / TOSEM; con dos manuscritos ya en EMSE, la
      conferencia sube de atractivo.
- [ ] **Opcional, bloqueado por RAM**: un sweep A/A dedicado afinaría
      la distribución nula. El banco actual alcanza para el núcleo.
