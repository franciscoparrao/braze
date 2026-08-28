# Outline honesto del Paper 3: gates de aceptación para optimización de harness bajo ruido

> **SUPERADO EN SU PREMISA (2026-08-28).** La frase "ninguno resuelve
> cómo decidir bajo ruido" quedó refutada por Parupudi (2608.21382) y
> Recuris (2608.24876), ambos del 25-26 de agosto, que sí miden. La
> evidencia propia y los threats de este documento siguen siendo
> válidos; la premisa, el posicionamiento y las contribuciones se
> reescribieron en `docs/paper3-outline-v2-2026-08-28.md`. Se conserva
> por trazabilidad del razonamiento, no como plan vigente.

Fecha: 2026-08-21
Estado: **outline — scoping de la versión que los datos YA sostienen**,
siguiendo la disciplina que funcionó con el Paper 2 (declarar qué
existe y qué no, antes de escribir una línea de manuscrito).

## La oportunidad (por qué ahora)

En cinco meses aparecieron **cuatro sistemas independientes** que
optimizan el harness alrededor de un modelo congelado, y **ninguno
resuelve cómo decidir bajo ruido**:

| Sistema | Regla de aceptación | Estadística |
|---|---|---|
| Meta-Harness (mar, Stanford/MIT) | mejor score en el search set | ninguna |
| AutoDesign (ago-13, Meituan/MBZUAI) | `J_train↑ ∧ J_dev no baja` | ninguna |
| HCL (ago-19, Nanjing/Wollongong) | `Δ ≥ δ` + retención en anchor set + validez | ninguna (seed fijo, 1 corrida) |
| LoopsBench (jul, Microsoft) | (mueve el foco al loop; no aborda el gate) | — |

HCL es el más avanzado: agrega **retención histórica** y nombra
**harness-level forgetting**. Pero su decisión sigue siendo un umbral
sobre una diferencia de estimaciones puntuales, y —crucialmente— el
resultado **se commitea al estado desplegado**, así que el ruido
aceptado contamina todas las evaluaciones posteriores y compone a lo
largo del loop.

Contrapunto que demuestra que la exigencia es razonable, no purista:
Spec-Driven Test Generation (Google, ago-17) evalúa **una** palanca de
harness con McNemar + bootstrap sobre 90 bugs reales. Se puede hacer.
La cultura de SE empírico ya lo hace; la de optimización de harness
todavía no.

## El claim que los datos SÍ sostienen

**En el régimen de modelos pequeños, el ruido de un banco agéntico es
del mismo orden que los efectos que estos gates buscan aceptar; una
regla de estimación puntual acepta ruido con probabilidad alta, y como
el harness se commitea, ese error se acumula.**

Evidencia propia ya medida (no hay que correr nada nuevo para el
núcleo):

1. **El falso positivo atrapado**: `seeded − baseline = +13` tareas,
   McNemar exacto p=0,011, **Holm-corregido 0,021**, cumpliendo un
   criterio de promoción pre-registrado — y el brazo de control con
   **prompt byte-idéntico** subió +9 por su cuenta; el único contraste
   que aísla el contenido (`seeded − empty`) es nulo (+4, p=0,541).
   *Todo gate de la tabla de arriba habría commiteado ese cambio.*
2. **El piso de ruido medido**: ~20% de las celdas pareadas voltean
   con prompts byte-idénticos (suite discriminante, temp 0,2).
3. **La miscalibración del propio umbral**: nuestro gate de plomería
   usó ≤2 celdas discordantes, importado del piso de una suite más
   fácil — **10× por debajo** del piso real. Un umbral prestado es un
   umbral inválido.
4. **El clustering**: el mismo +13 no sobrevive al tratar las
   repeticiones de una tarea como no independientes (sign test
   p=0,065; Wilcoxon p=0,054) — *antes* de consultar el control.
5. **La deriva temporal** (2026-08-20): correr brazo-por-brazo
   confunde deriva del nodo con tratamiento (timeouts 6/5/2 → 4/17/17
   en el orden de ejecución). El orden de ejecución es parte del gate.
6. **MDE**: con ~21 pares discordantes de ruido, un McNemar exacto
   exige asimetría neta ≥11 pares (~11 pp) para p<0,05 a 3
   repeticiones. Efectos menores son invisibles, y decir eso por
   adelantado es parte del método.

## Contribuciones propuestas

1. **Formalizar el problema del gate de commit** para harness
   continuo: qué garantiza (y qué no) una regla de decisión sobre
   estimaciones puntuales cuando la métrica tiene ruido de banco.
2. **Distribución empírica del ruido de un banco agéntico**,
   construida del propio archivo del proyecto: **92 sweeps
   commiteados** con réplicas de configuración idéntica (repeticiones,
   seeds, y el par baseline/empty con prompt idéntico por
   construcción). Es un activo que no existe públicamente.
3. **EL EXPERIMENTO CENTRAL — tasa de aceptación falsa de las reglas
   publicadas**: aplicar las reglas de decisión de Meta-Harness,
   AutoDesign y HCL a pares de configuración *idéntica* extraídos del
   banco, donde por construcción no hay efecto, y **medir con qué
   frecuencia aceptan**. No es argumentación: es una tasa de error
   medida sobre datos reales. Se puede correr **sin hardware nuevo**.
4. **Un gate de referencia y su costo**: pre-registro + contraste
   pareado exacto + corrección de multiplicidad + piso in-sweep con
   brazo de configuración idéntica + MDE declarado + orden intercalado
   + corte secuencial anytime-valid (`sequential.rs`) — con el precio
   explícito en corridas, que es la objeción obvia y hay que
   responderla con números.
5. **Caso de estudio completo** del falso positivo: el único ejemplo
   publicado (que sepamos) de un resultado de harness que pasa
   pre-registro, McNemar y Holm, y aun así se disuelve contra un
   control de prompt idéntico.

## Qué existe y qué NO (honestidad de alcance)

**EXISTE**: los 92 JSONs con sus metadata de entorno; los 9
pre-registros con criterios declarados antes de correr; el pm-ab
completo con su control y su diagnóstico de sesiones preservadas; los
pisos medidos; `sequential.rs`; la métrica dual; el DBV; y el
historial de nulos (planner en prosa, stencil ×3, edit-fence, TTC,
lead-summary) que da contexto de cuántas palancas "obvias" mueren al
medirlas.

**NO EXISTE (y hay que decirlo)**:
- **No reimplementamos sus sistemas.** Simulamos sus *reglas de
  decisión* sobre nuestra distribución de ruido. La crítica es a la
  regla, no al sistema completo — y así debe redactarse, sin
  excepción. Es el threat #1.
- **Un sweep A/A dedicado** con muchas réplicas afinaría la
  distribución nula. El banco actual alcanza para el núcleo; un A/A
  dedicado (~5-8 h de Nitro sano) lo fortalecería. **Bloqueado por
  memoria** hasta la ampliación de RAM.
- **No tenemos un loop de optimización propio**: no demostramos que
  *nuestro* gate produzca mejores harnesses a lo largo de un loop
  largo — solo que el suyo acepta ruido y el nuestro lo detecta.
  Declarar como future work, no insinuar lo contrario.

## Relación con los papers 1 y 2

- **Paper 1** (en submission EMSE): mide palancas una por una y
  descubre que muchas "obvias" son nulas o dañinas en escala chica —
  *provee el catálogo de efectos reales* contra el cual calibrar qué
  magnitudes deben ser detectables.
- **Paper 2** (congelado): mide la memoria y aporta el **control del
  mismo prompt** como instrumento. El Paper 3 lo generaliza de
  instrumento puntual a **componente obligatorio del gate**.
- **Paper 3**: la maquinaria de decisión. Los tres forman un programa:
  qué palancas importan (1), qué cuesta la memoria (2), cómo decidir
  sin engañarse (3).

## Threats to validity (esbozo)

- **Simulación de reglas ajenas** (el principal, arriba).
- **Un régimen**: modelos locales 3-20B, suites Rust con oráculo
  `cargo check`, un nodo. Los gates de ellos operan en régimen
  frontier con benchmarks distintos; la tasa de aceptación falsa
  depende del ruido, y el ruido depende del régimen. El claim debe
  acotarse a "régimen ruidoso" y ofrecer el método para que cualquiera
  mida el suyo — no afirmar que sus resultados publicados son falsos.
- **Nuestro propio banco como fuente del ruido**: circularidad
  parcial; mitigar reportando el ruido por configuración y no un
  número único.
- **Deriva del nodo** dentro del archivo histórico (descubierta el
  20-ago): parte del "ruido" del banco es deriva, no sampling —
  reportarlo separado donde el diseño lo permita.

## Venue (decisión en su momento con `/paper-match`)

Candidatos: EMSE otra vez (encaja: metodología empírica + resultados
negativos + infraestructura de investigación), pero ya tendríamos dos
manuscritos ahí; **ICSE/FSE** como alternativa de conferencia (timing
más rápido, y la crítica metodológica encaja en su tradición);
**TOSEM** si el paquete queda grande. La decisión formal, con el
manuscrito en mano.

## Próximo paso concreto

El orden que minimiza riesgo (igual que el Paper 2): **empezar por el
experimento central**, que es re-análisis de datos existentes y no
necesita hardware. En concreto:

1. Inventariar del banco todos los pares de **configuración idéntica**
   (mismo backend, mismas ablaciones, mismo prompt) y construir la
   distribución empírica de Δ bajo H0.
2. Implementar las tres reglas publicadas como funciones de decisión.
3. Medir su tasa de aceptación sobre esos pares.
4. Recién con ese número en mano, decidir si el paper se escribe.

Si el paso 3 arroja una tasa alta (mi prior: alta), el paper tiene su
resultado central medido antes de escribir una sola sección.
