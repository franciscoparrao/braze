# Pre-registro: ¿la INDEPENDENCIA del aceptador rescata la palanca de verificación?

Fecha: 2026-09-02
Estado: **DISEÑO. Ninguna corrida lanzada.**
Antecedente directo: `docs/sweep-verification-lever-ab-powered-2026-07-22.md` (H2, REJECT)
Disparador externo: Harness-of-Harness, arXiv 2609.01481 (Shanghai AI Lab, 2026-09-01)

## Por qué re-abrir una palanca ya rechazada

El A/B potenciado de la palanca de verificación salió **REJECT** con un nulo
resonante (McNemar p=1.0 / p=0.73): el gate recuperaba aproximadamente
tantas tareas como rompía. Aquel diseño ponía **al mismo modelo a
verificar su propio trabajo dentro del mismo turno**.

HoH (§3.4) propone un mecanismo distinto para el mismo problema:

> "Acceptance must instead be determined from observations of a fixed
> candidate by a role that did not produce that candidate."

Es decir: lo que haría funcionar la verificación no sería la **capacidad**
del verificador sino su **independencia** respecto de quien produjo el
candidato. HoH obtiene +52,25% de ganancia relativa media **con el mismo
modelo en los tres roles** (Planner / Developer / QA Tester), lo que aísla
la independencia como el factor activo: si fuera capacidad, usar el mismo
modelo no aportaría nada.

Esto convierte el nulo del H2 en una hipótesis nueva y falsable, no en una
excusa para reintentar lo mismo.

## Lo que NO sirve de esta infraestructura, y por qué

braze ya tiene el par de subagentes completo: `explore` (Viewer,
read-only) y `editor` (SWE-Edit, `393748e`). **No son un aceptador
independiente.** Existen para mantener el churn —ediciones fallidas,
volcados de archivo, salida de `cargo check`— fuera del contexto del
padre. Aíslan *contexto*, no *autoría*: el padre sigue siendo quien
decide que el trabajo está hecho.

Lo que falta es un rol que juzgue un candidato fijo **sin haber
participado en producirlo y sin ver la traza de su producción**. Se
construye sobre el mecanismo de subagentes existente, pero es un rol
nuevo, no una reutilización.

## Hipótesis

**H1.** Una ronda de aceptación por un rol independiente —misma
configuración modelo/harness, invocación separada, que ve el artefacto
final y el objetivo pero NO la traza de implementación— mejora el pass
rate frente al control sin verificación.

**H0.** El efecto es nulo, como en el H2: la independencia no cambia el
balance recupera/rompe.

## Diseño

Tres brazos, suite discriminante v2 (34 tareas), un ejecutor local:

| brazo | qué hace |
|---|---|
| `control` | turno normal, sin verificación |
| `self-verify` | replica del H2: el mismo modelo verifica su trabajo, con la traza a la vista |
| `independent-accept` | invocación separada que recibe solo (objetivo, artefacto final) y devuelve accept/reject + razón; un reject dispara una ronda de reparación |

`self-verify` se re-corre en vez de citarse: el H2 se midió en otra suite
y con otro binario, así que sin él la comparación mezclaría el efecto con
esas dos diferencias.

**La distinción crítica del brazo 3 es informacional, no de capacidad**:
mismo modelo, mismo harness, pero el aceptador NO ve cómo se llegó al
artefacto. Si la ganancia viniera de "una ronda más de cómputo", el brazo
2 la capturaría también.

## Métricas

| métrica | rol |
|---|---|
| `pass_rate` | primaria |
| `recovered` / `broken` | el balance que el H2 encontró en empate — descompone el neto |
| tokens y rondas totales | el brazo 3 agrega llamadas; una ganancia que cueste el doble de contexto choca con la frontera de amortización del Paper 2 |
| tasa de reject del aceptador | mecanismo: un aceptador que nunca rechaza no está haciendo nada |

## Criterio comprometido antes de correr

- **`independent-accept` supera al control con IC que excluye cero, y su
  balance recupera/rompe es netamente positivo → ADOPTAR** y reportar
  que la independencia era el factor que le faltaba al H2.
- **Empate con el control, pero mejor que `self-verify` → resultado
  parcial**: la independencia ayuda pero no basta; se reporta como matiz
  del H2, sin adoptar la palanca.
- **Empate con `self-verify` → H0.** El nulo del H2 se confirma y se
  extiende: no era la falta de independencia. Se publica como refuerzo
  del resultado anterior, no como fracaso nuevo.

Cláusula anti-racionalización: si sale H0 **no** se reintentará con un
modelo más grande de aceptador. Esa sería la hipótesis de *capacidad*,
que es justamente la que HoH descarta al obtener sus ganancias con el
mismo modelo en los tres roles; convertirla en la explicación de rescate
sería mover el poste.

## Amenazas a la validez, anotadas antes

- **HoH corre sobre modelos frontera** (GPT-5.5, DeepSeek-V4-Pro,
  MiniMax-M3). Dos resultados independientes recientes —WikiSkill y la
  re-verificación del gradiente— apuntan a que este tipo de andamiaje
  rinde **más** cuanto más capaz es el ejecutor. En el régimen SLM de
  braze, H0 es un desenlace plausible y no debe leerse como refutación
  de HoH.
- El aceptador comparte los sesgos del productor por ser el mismo modelo:
  la independencia es informacional, no epistémica.
- La suite discriminante puntúa por aserciones; un aceptador que aprenda
  a predecir la aserción mide otra cosa. El prompt del aceptador no debe
  incluir las aserciones, solo el objetivo en lenguaje natural.
- Costo: tres brazos × 34 tareas × repeticiones, con el brazo 3 añadiendo
  una invocación por turno. Presupuestar antes de lanzar (el sweep del
  gradiente tardó 1h42 por ejecutor y terminó en OOM).
