# Outline: paper de seguridad del perímetro (línea transversal del framework)

Fecha: 2026-08-11
Estado: **esqueleto — scoping, no draft.** Reúne la evidencia dispersa de
la capa de seguridad de braze en una narrativa candidata, y evalúa
honestamente si da para un paper propio o es una sección fuerte de otro.
Origen: la línea "Privacidad local-cloud / security" del framework
(`docs/research-discipline-framework-2026-07-16.md`), marcada
*transversal, futura* — pero el TRABAJO ya existe (bwrap, gate sintáctico,
paquete de hardening v9), así que la pregunta es de encuadre, no de si
hay material.

## La pregunta de encuadre (a resolver ANTES de escribir)

Los mecanismos individuales —Landlock, seccomp, bubblewrap, un
clasificador de permisos— son **técnicas estándar**. Un paper que solo
diga "sandboxeé un coding agent con bwrap" no aporta. Lo potencialmente
publicable es más fino, y hay que ser honesto sobre cuál de estos tres es
el verdadero:

1. **El modelo de amenaza específico del MODELO CHICO.** Un agente cloud
   asume un modelo competente que rara vez alucina un `curl | sh`. braze
   maneja modelos que **alucinan comandos peligrosos por incapacidad, no
   por malicia** (el incidente roam: `search` inexistente 3×; gpt-oss
   emitiendo comandos rotos). El harness NO puede delegar la seguridad al
   juicio del modelo — DEBE ser el límite. Ese desplazamiento del
   threat-model (de "modelo adversario" o "modelo confiable" a "modelo
   incapaz cuyo error es el vector") es lo que puede ser novel.

2. **El razonamiento por LÍMITE DE MECANISMO como método de diseño.**
   Cada capa de braze documenta explícitamente *qué no puede hacer y por
   qué existe la siguiente*: Landlock es allowlist-only → no puede negar
   lectura de secretos → por eso el mount namespace de bwrap. Esa cadena
   "conocé el límite de tu mecanismo y compón la siguiente capa sobre ese
   límite exacto" es una disciplina de diseño articulable, vs seguridad
   ad-hoc por acumulación de features.

3. **La disciplina de VERIFICACIÓN EN VIVO de claims de seguridad.**
   Cada capa se probó contra el ataque real, no se asumió: bwrap en Nitro
   **cazó un bug** (el mask file no idempotente que habría dejado
   secretos legibles — un compilaba-y-pasaba-tests que solo la corrida
   real reveló); el gate sintáctico se midió contra roturas reales. Claims
   de seguridad TESTEADOS, no aseverados — raro y valioso.

**Veredicto de scoping (honesto)**: los tres juntos dan un **"design +
measurement / experience report"**, no un estudio empírico con hipótesis
falsable. Es más débil que Paper 1 (que sí tiene curva harness-vs-escala
falsable). Dos caminos:
- (A) **Sección de seguridad DENTRO del paper de harness-engineering**
  de seguimiento — probablemente el mejor hogar: las 5 features del
  survey + la capa de seguridad como un capítulo de "el harness como
  límite para modelos incapaces".
- (B) **Paper corto de systems/security** (workshop) si el threat-model
  del modelo-incapaz se puede sostener como contribución central con
  algún experimento adversarial (métricas del framework:
  `outside_workdir_attempts`, `sensitive_pattern_blocks`,
  `denied_actions` — hoy NO medidas sistemáticamente).

Recomendación: (A) por defecto; (B) solo si el experimento adversarial se
diseña y corre (hoy no existe — es trabajo nuevo, no solo escritura).

## Las capas (el contenido técnico, ya construido y verificado)

Arquitectura en capas, cada una con su límite documentado:

1. **Modelo de permisos de dos capas** (`braze-permissions`):
   clasificador (léxico/semántico de la ACCIÓN: Reversible vs
   Irreversible) + guard (confirmación humana o caché). Límite conocido:
   el clasificador es **poroso** (K-2/J-20/J-31: symlinks, rutas
   creativas, subprocesos de shell burlan la clasificación de la
   *descripción*).

2. **Hardening in-process del proceso** (paquete v9, `f44edf8`,
   `sandbox.rs`): `prctl` (no core dumps, no new-privs, scrub de
   `LD_PRELOAD`) + **seccomp** (deniega `io_uring_*`/`ptrace`/
   `process_vm_*` — las clases de bypass que Landlock no cubre) +
   **Landlock write-only** (`3d4c6b3`: restricción de KERNEL de escrituras
   a una allowlist). Límite conocido y DOCUMENTADO: Landlock es
   allowlist-only sin reglas de deny → **exfiltración-abierto** (el agente
   puede leer `~/.ssh`, `.env`) y no puede hacer `.git/hooks` read-only
   bajo un workdir escribible.

3. **`.git`/`.braze` read-only en el CLASIFICADOR** (`200a020` K-3, y v9):
   la defensa de escalación en la capa correcta — una escritura del
   modelo bajo `.git/` (un hook, `core.hooksPath`) corre código arbitrario
   en el próximo comando git; irreversible por naturaleza, no un archivo
   del workdir. Vive en el clasificador porque Landlock no puede
   expresarlo.

4. **Sandbox out-of-process por-tool con bubblewrap** (`714bba5`, design
   `docs/bwrap-tool-sandbox-design-2026-08-10.md`): mount namespace que
   expresa las DOS cosas que Landlock no puede — read-denial de secretos
   (`.env` enmascarado con archivo `chmod 000`) y `.git` read-only bajo
   workdir escribible. Cierra la exfiltración que la capa 2 deja abierta.
   Límite: opt-in, Linux, requiere user namespaces (falla en entornos
   anidados — verificado).

5. **Gate sintáctico pre-aplicación** (`2e9a3e5`, Tier-1 del survey):
   reject-before-apply — una edición que rompería la sintaxis de un `.rs`
   se rechaza ANTES de escribir. No es "seguridad" clásica pero es
   integridad: el modelo incapaz no puede dejar el repo en estado roto de
   forma silenciosa.

## El mapa de posicionamiento (contra los 5 repos del survey)

- **codex** (`linux-sandbox`): Landlock ABI≥1 read-scoping (read-denial
  nativo, sin bwrap), seccomp de sockets (`AF_UNIX` solo con red off),
  execpolicy TOML declarativo con **hashing de integridad** y reglas
  self-testing. Es la referencia más madura; braze toma el bwrap por-tool
  (más portable que Landlock-read que exige ABI≥1) pero le falta el
  hashing de política y el execpolicy self-testing (diferidos, anotados).
- **gemma/gemini-cli**: bubblewrap por-tool (el blueprint de braze),
  exfiltración-CERRADO vía enmascarado de secretos — braze lo replicó.
- **Contraste con grok-build**: auto-aprueba cuando el sandbox del SO está
  activo ("el sandbox hace imposibles los malos resultados"). braze
  predice-y-pregunta (portable, probabilístico) PERO ahora también tiene
  la capa de encierro — el ángulo nuevo (survey #13): si Landlock/bwrap
  está OFF o falló, la política de confirmación debe *apretarse*, no
  quedar permisiva. braze hace fail-closed en fallo de APLICACIÓN de
  Landlock; el apretar-confirmación-si-sandbox-off es futuro.

## Threats to validity (la mitad honesta)

- **Landlock write-only es exfiltración-abierto** sin bwrap; bwrap es
  opt-in + Linux + userns. En un entorno sin userns (contenedor anidado),
  la única defensa contra lectura de secretos es… ninguna in-process.
- **El clasificador léxico es poroso** por construcción (clasifica la
  descripción, no el efecto). Landlock/bwrap lo respaldan, pero fuera de
  Linux el gate léxico es todo lo que hay.
- **Sin hashing de integridad de política** (execpolicy): una regla de
  clasificador rota se shippea sin un test que la cace al cargar.
- **Las métricas de seguridad del framework NO se miden sistemáticamente**
  (`outside_workdir_attempts`, `sensitive_pattern_blocks`,
  `denied_actions`): sin un experimento adversarial, los claims son de
  DISEÑO, no empíricos. Este es el gap que decide (A) vs (B).

## Qué falta para que sea (B), un paper con experimento

Un banco adversarial: prompts que inducen al modelo a (a) leer un secreto
plantado, (b) escribir fuera del workdir, (c) reescribir un git hook, (d)
un comando colgante — medido con y sin cada capa. Las métricas del
framework serían las variables dependientes. Es **trabajo nuevo** (diseño
de banco + sweeps), no escritura — por eso la recomendación por defecto es
(A), la sección, que usa la evidencia que YA existe (los designs + las
verificaciones en vivo).

## Próximo paso concreto

Si se elige (A): este outline se convierte en la sección "Security &
Integrity: the harness as the boundary for an incapable model" del paper
de harness-engineering de seguimiento, junto a las otras 4 features del
survey. Si (B): pre-registrar el banco adversarial primero (mismo rigor
que los A/B de round-economics), correrlo, y recién ahí escribir.
