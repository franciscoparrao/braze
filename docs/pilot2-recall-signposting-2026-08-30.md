# Piloto 2 — resultados: el índice NO basta, y mi instrumento estaba mal

Fecha: 2026-08-30
Pre-registro: `docs/hypothesis-2026-08-30-recall-signposting.md` (antes de correr)
Datos: `docs/pilot2-recall-signposting-ornith9b-2026-08-30.json`
Ejecutor: `ornith:9b` (Nitro). 3 tareas × 2 brazos × 5 reps = 30 corridas, 4 timeouts.
Estado: **CERRADO.**

## Primero: un error de instrumentación que afecta también al piloto 1

El pre-registro definió la métrica primaria como "fracción de corridas donde
el modelo **leyó** algún archivo de memoria". El runner la implementó como
"cualquier tool call cuyos argumentos mencionen `project-memory`", con un
comentario que lo justificaba ("un grep/shell sobre el directorio es la
misma conducta"). **No es la misma conducta**, y los datos lo prueban:

Herramientas que tocaron `project-memory/` en el piloto 2:

| Herramienta | Veces | ¿Entrega contenido? |
|---|---|---|
| `read_file` | 12 | **Sí** |
| `glob` | 13 | **No** — solo lista nombres de archivo |

Más de la mitad de las "consultas" fueron `glob {"pattern":
"project-memory/*.md"}`: el modelo **vio que la memoria existía y no la
leyó**. Se detectó al auditar por qué corridas marcadas como "consultó" no
cumplían la convención; el código final las delata —
`AppError::Invalid(0)` en vez del `from_code(422)` que la memoria
especifica — porque nunca vieron el contenido.

Es la misma clase de arista MODEL—BENCH que el proyecto ya documentó dos
veces (gemma4:e4b vía `shell_exec`, ornith:9b vía read+write): el
instrumento midió una ruta y no el logro.

Todos los números de abajo usan la definición **estricta** (`read_file`
sobre un archivo de memoria), que es la fiel al pre-registro.

### Corrección al piloto 1

| | reportado (laxo) | corregido (estricto) | ¿cambia el veredicto? |
|---|---|---|---|
| gpt-oss:20b | 12/12 (1.00) | 12/12 (1.00) | No |
| ornith:9b | 11/14 (0.786) | **10/14 (0.714)** | No — sigue > 60% |

El veredicto del piloto 1 (SEGUIR) se sostiene. Su número de ornith debe
citarse como **71,4%**, no 78,6%.

## Resultado del A/B

| Brazo | n | `recall_invocation_rate` (estricto) | cumplimiento |
|---|---|---|---|
| `neutral` | 14 | **0.36** (5/14) | 6/14 |
| `signposted` | 12 | **0.58** (7/12) | 6/12 |

Por celda (recall estricto):

| Tarea | neutral | signposted |
|---|---|---|
| `errors` | 1/5 | 3/5 |
| `logging` | 3/4 | 3/4 |
| `tests` | 1/5 | 1/3 |

## Veredicto: H1 rechazada; la premisa del diseño V2 no se sostiene

**H1** ("con el índice presente, un prompt neutro activa la consulta con
tasa comparable a uno señalizado") **se rechaza**: 0.36 contra 0.58.

Y contra el criterio comprometido antes de correr — neutro < 40% —
corresponde la conclusión más dura de las tres que estaban escritas:

> El índice NO cumple su función. El diseño V2 no se rechaza entero, pero su
> premisa central sí: habría que activar la consulta por otro medio, y eso
> vuelve a pagar costo por ronda — es decir, vuelve a chocar con R1 y hay que
> rediseñar, no parchar.

El agravante es que **ni siquiera el brazo señalizado alcanza el umbral**
(0.58 < 0.60). Con la memoria anunciada en el system prompt *y* el usuario
pidiendo explícitamente seguir las convenciones del proyecto, `ornith:9b`
lee la memoria poco más de la mitad de las veces. La conducta modal del
resto es mirar el índice, listar el directorio, y resolver con sus propios
supuestos.

## El hallazgo conductual, que es lo que hay que llevarse

**El modelo reconoce que la memoria existe y decide no consumirla.** No la
ignora —13 globs son evidencia de que la vio y fue a mirarla— pero se
detiene en el listado. El índice logra la mitad de su trabajo (señalizar) y
falla en la otra mitad (motivar la lectura).

Eso descarta la explicación cómoda ("el índice es muy chico y no se nota") y
apunta a otra: para un modelo de 9B, el costo percibido de una ronda extra
de lectura compite con seguir adelante con lo que ya cree saber. Es la misma
economía de rondas que el Paper 2 encontró por el lado del costo de tokens,
apareciendo ahora por el lado de la decisión del modelo.

Nótese la asimetría con gpt-oss:20b, que leyó 12/12 en el piloto 1: **la
memoria bajo demanda funciona en el modelo que menos la necesita.** Es,
estructuralmente, el mismo patrón que el Paper 2 reportó para el playbook
(amortiza solo donde es redundante). Dos mecanismos distintos, el mismo
sesgo.

## Qué NO se puede concluir

- No se midió `net_token_delta`. Esto sigue sin decir nada sobre si la
  memoria amortiza.
- n=5 por celda: efecto grosero, no matiz. La diferencia 0.36 vs 0.58 con
  n=14/12 no es un intervalo que soporte peso.
- Un solo ejecutor y una sola operacionalización de "señalizar".
- Los 4 timeouts se concentran en `logging` y `tests/signposted`; la celda
  `tests/signposted` queda con n=3.

## Consecuencia para el diseño

El paso 2 del diseño (esquema + store + `recall_memory`) **queda
bloqueado**: construir la herramienta no arregla que el modelo elija no
usarla. Las salidas posibles, en orden de coste:

1. **Aceptar la asimetría** y declarar la memoria bajo demanda como palanca
   para ejecutores capaces (≥20B), documentando que en 9B no activa. Barato
   y honesto, pero contradice el objetivo declarado del proyecto (modelos
   chicos).
2. **Cambiar el vehículo de activación** — inyección en la primera ronda del
   turno, o un hook que fuerce la lectura. Vuelve a pagar por ronda: hay que
   medirlo contra R1 antes de escribirlo, no después.
3. **Publicar el nulo** y cerrar la línea, sumándolo a los tres nulos limpios
   que el proyecto ya tiene (stencil, palanca de verificación, edit-fence).

Ninguna se elige acá: es decisión del autor.
