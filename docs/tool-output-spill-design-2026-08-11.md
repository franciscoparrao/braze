# Diseño: truncado head+tail + spill-to-file de tool output

Fecha: 2026-08-11
Origen: survey de referencia (`docs/reference-agents-survey-2026-08-10.md`
§ gemini-cli, "truncado de tool-output por presupuesto-de-tokens-inverso
con spill-to-file"). Mecanismo extraído del clon
(`packages/core/src/context/truncation.ts`).

## El problema

Un `grep -r` sobre un repo grande, un `cargo build` que falla, un log
largo: el output desborda el `output_budget` (8000 bytes) y braze hoy
hace dos cosas subóptimas:

1. **Se queda solo con el HEAD** (`truncate_output` corta en `budget` y
   descarta el resto). Pero en grep/logs/builds el dato que importa está
   con frecuencia al FINAL — el error de compilación, el último match, el
   resumen. El head-only lo tira.
2. **Descarta el resto para siempre**: el modelo ve "narrow your query"
   y tiene que re-correr un comando caro con un patrón más específico —
   adivinando qué acotar sin haber visto lo que se perdió.

## Las dos mejoras (componen)

### A. Head+tail proporcional (siempre-on, reemplaza el head-only)

En vez de conservar solo los primeros `budget` bytes, conservar el
**head (20%) y el tail (80%)** con un marcador en el medio. El error al
final del build sobrevive; el inicio (que suele decir QUÉ corrió) también.
Ratio 0.2 head / 0.8 tail, del blueprint (`headRatio = 0.2`) — el tail
pesa más porque es donde vive el resultado/error en el caso típico.
Determinístico, sin archivos, sin config: es estrictamente mejor que el
head-only actual.

El cap por líneas (`output_max_lines`) se aplica ANTES, igual que hoy
(un `grep -r` de miles de líneas cortas cabe en bytes pero no en
contexto); el head+tail opera sobre el resultado.

### B. Spill-to-file (gateado, sin pérdida)

Cuando el output se trunca Y el spill está habilitado, escribir el output
**completo** (pre-truncado) a `.braze/spill/<call_id>.txt` bajo el
workdir, y anexar al resultado truncado:

```
Full output (N bytes) saved to .braze/spill/<call_id>.txt —
read specific ranges with read_file (offset/limit) instead of
re-running this command.
```

El modelo recupera exactamente lo que necesita con `read_file` paginado
(que ya soporta `offset`/`limit` por líneas) en vez de re-correr el
comando. Sin pérdida: nada se tira, solo se mueve fuera del contexto
inmediato.

**Por qué `.braze/spill/` bajo el workdir**:
- Un `read_file` de un path bajo el `WorkdirAllowlist` es `Reversible`
  → silencioso, sin prompt (verificado en `classifier.rs:140`). Un spill
  en `/tmp` quedaría fuera del allowlist y pediría confirmación (y el
  bench, sin prompter, lo denegaría).
- La protección `.braze`/`.git` del clasificador es solo para
  ESCRITURAS del modelo (`classifier.rs:112`); las LECTURAS de `.braze`
  pasan. Y el harness escribe el spill DIRECTO (`std::fs`, no vía la tool
  `write_file`), así que no pasa por el guard en absoluto.
- Cae con el sandbox del bench (dir por-tarea) — sin basura persistente.

## Alcance MVP y decisiones

ENTRA: A siempre-on; B gateado por `Config::enable_tool_output_spill`
(default **on** — es sin pérdida y el path es leíble sin fricción; la
doctrina "off hasta validar" aplica a palancas que cambian el
RAZONAMIENTO, y esto solo cambia DÓNDE vive el output, no qué decide el
modelo). El bench lo puede apagar con `+ablate:no-spill` si su A/B lo
pide.

DECISIONES:
- **Sin estimación de tokens**: braze trunca por BYTES determinísticos
  (el `output_budget` existente). La "inversión de presupuesto de tokens"
  del blueprint es su forma de ajustar a un budget fijo; braze ya ajusta
  a un budget fijo por bytes. No se reimplementa el estimador
  ASCII/no-ASCII — sería complejidad sin señal.
- **`call_id` como nombre de archivo**: único por llamada, ya sanitizado
  (los ids del engine son `uuid`/`rescued-…`). Un turno que trunca dos
  veces la misma tool genera dos spills distintos.
- **Limpieza**: los spills viven en `.braze/spill/`. En el CLI persisten
  (el usuario puede querer inspeccionarlos); no crecen sin control porque
  solo se escriben al truncar. Una limpieza al inicio de sesión se puede
  agregar después — MVP no la incluye (el dir es del proyecto, borrarlo
  agresivo podría pisar un spill que el usuario está mirando).
- **Fallo de escritura del spill degrada a solo-truncado**: si `.braze/`
  no es escribible, se cae al comportamiento A (head+tail sin spill) con
  el trailer "narrow your query" — nunca aborta la tool.

DIFERIDO:
- Limpieza automática de spills viejos.
- Spill para el output que la COMPACTACIÓN tactical dropea (otra fuente
  de pérdida, otra costura — el engine, no el provider).

## Dónde vive

`truncate_output` en `braze-tools-local/src/provider.rs` gana el
head+tail. El spill necesita el `call_id` y el `workdir` (que el provider
ya tiene), así que la lógica de spill vive en `wrap` (que tiene el
`call`) o en una variante de `truncate_output` que reciba ambos. El
provider gana un campo `spill_enabled: bool` + builder, cableado desde
`Config::enable_tool_output_spill`.

## Verificación

- **Unit**: head+tail conserva ambos extremos y marca el medio; un output
  bajo el budget pasa intacto; el cap de líneas se aplica antes.
- **Integración**: una tool cuyo output desborda el budget → el resultado
  truncado apunta a `.braze/spill/<id>.txt`, y ese archivo contiene el
  output COMPLETO. Con el spill off, el trailer vuelve al "narrow your
  query" sin archivo.
- **En vivo**: un `grep`/`shell_exec` real con output grande; confirmar
  que el archivo de spill existe con el contenido completo y que un
  `read_file` con offset/limit lo recupera.
  **VERIFICADO (2026-08-11)** con el binario real: `cat big.txt` (424 KB,
  6000 líneas) vía `shell_exec` desbordó el budget, el spill se escribió
  a `.braze/spill/<id>.txt` con el output completo (su cola contiene la
  línea 6000), y el modelo (deepseek-v4-flash) respondió correctamente
  cuál era la última línea — el dato vive al FINAL del output, que el
  head-only anterior habría descartado. La mejora se demostró
  end-to-end: el modelo vio en contexto (vía el tail) lo que antes
  habría exigido re-correr el comando o adivinar.
