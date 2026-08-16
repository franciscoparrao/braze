# Hipótesis: replicación del piloto M1 en un segundo executor (ornith:9b)

Fecha: 2026-08-15
Estado: proposed — este documento se commitea ANTES de lanzar el sweep
(registro git-only, convención del proyecto)
Línea: Paper 2 (cierre S4) — responde el issue R1 del review EMSE simulado
(`~/vault/journals/emse/reviews-generated/2026-08-15-paper2-amortization-frontier.md`):
la base de evidencia del Study 1 es un solo executor. El autor autorizó
explícitamente este sweep el 2026-08-15 (excepción puntual a la regla
"NO correr sweeps nuevos para este paper" del outline).

## Pregunta

¿El patrón del piloto M1 —el playbook humano ahorra rondas solo en la
tarea saturada y cuesta tokens netos en las frescas (la anti-correlación)—
replica direccionalmente en un segundo executor de arquitectura distinta
(ornith:9b, denso 9B, vs gpt-oss:20b, MoE ~3.6B activos)?

## Diseño (idéntico al M1 original salvo el executor)

| | |
|---|---|
| Suite | `crates/braze-bench/suites/memory-distillation.toml` (7 brazos-tarea: 3 pares none/playbook + holdout) |
| Executor | `ollama:ornith:9b` (denso 9B, digest actual en Nitro) |
| Nodo | Nitro (`BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434`), verificado ocioso al lanzar |
| Repeticiones | n=20 por celda: 4 tandas de 5, seeds base 42/47/52/57 (rango 42–61, igual que el original) |
| Flags | `--no-ollama-stop` (permitido: sweep de modelo único) |
| Total | 140 corridas |

## Hipótesis principal (direccional, pre-registrada)

H1: la estructura de la Ec. (1) replica en dirección: ΔR es mayor en la
tarea que ornith:9b tenga saturada que en las frescas, y `net_token_delta`
es positivo (cuesta tokens) en las tareas donde ΔR < ΔR*.

**Matiz declarado de entrada**: qué tarea está "saturada" depende del
executor. Para gpt-oss:20b fue `original`. Para ornith:9b se determina
del brazo `none` (pass rate y rondas), ANTES de leer los brazos playbook
— el patrón que la hipótesis predice es relativo a esa clasificación,
no a la de gpt-oss. Si ornith satura las tres tareas (95/95 en
default.toml sugiere capacidad alta), el pronóstico es ΔR chico en todas
y net_token_delta dominado por el costo fijo — también consistente con
la condición, y se reporta como tal.

## Hipótesis nula

H0: el patrón no replica — p.ej. ΔR grande en tareas frescas (la memoria
enseña, no recuerda), o net_token_delta negativo generalizado. Cualquiera
de las dos REABRE la pregunta y el paper debe reportarlo y matizar el
claim de generalidad — el resultado se publica igual, en la dirección
que salga.

## Métricas

Las del M1: pass, rounds, input_tokens, wall; derivadas ΔR, ΔT
(tokens/ronda), net_token_delta por par; CIs Welch al 95%, Holm entre
los 3 pares por endpoint (la corrección que el reanálisis del 2026-08-15
agregó al Study 1 original). Fisher + Newcombe para pass.

## Criterios de decisión, pre-registrados

1. **Replica** (esperado): el signo de la anti-correlación se sostiene
   bajo la clasificación saturada/fresca propia de ornith → el paper
   gana la subsección "Replication on a second executor" y el threat de
   modelo único se rebaja a "dos executors de arquitectura distinta,
   una familia de tareas".
2. **No replica**: se reporta como hallazgo en la misma subsección, el
   claim de generalidad del abstract se restringe explícitamente a
   gpt-oss:20b, y el threat se mantiene con el dato nuevo.
3. **Sin iteración**: una sola pasada de 140 corridas; fallos de
   infraestructura (HarnessError/CircuitOpen) quedan fuera del
   denominador según la convención del bench, y si superan el 10% del
   total el sweep se declara inválido y se repite completo (una vez)
   antes de leer nada.

## Riesgos anotados

- ornith:9b podría no soportar tools vía Ollama en alguna forma rara —
  smoke de 1 tarea antes de las tandas (no cuenta como iteración: es
  chequeo de instrumento, criterio idéntico al del A/B de agosto).
- Ollama no bit-exacto con seed fijo (documentado): los CIs y la
  dirección son el resultado, no los valores puntuales.
- 5.6 GB en Nitro: cabe entero; `--no-ollama-stop` no apila residentes
  (modelo único).

## Resultados (2026-08-16)

Sweep completo: 140 corridas, gate de infraestructura PASA (6.4% < 10%).
Análisis completo en `docs/sweep-m1-ornith9b-2026-08-15.md`.

**Veredicto: criterio 1 — REPLICA, y profundiza.** La clasificación
saturada/fresca de ornith (paso 1, solo brazos none: original 16/20 >
loop 12/20 > move 10/20) coincide con la de gpt-oss. El orden de la
anti-correlación se sostiene: daño mínimo en la sabida, máximo en la
más fresca. Pero el régimen es más severo: ΔR es NEGATIVO en los 3
pares (el playbook alarga trayectorias: −0.50 / −1.55 [Holm <0.001] /
−1.75 [Holm 0.049]) y el pass rate se degrada con significancia en las
frescas (12→5, Fisher 0.05; 10→2, Fisher 0.01). Net tokens +2709 /
+5794 / +6635, los 3 CIs excluyen cero. Holdout 18/20 sin descarrilar.
La censura por timeout (6/8 en brazos playbook) subestima el daño —
sesgo conservador.

```text
Decision: el patrón replica bajo el criterio pre-registrado 1; el
  paper gana la subsección de replicación y el threat de modelo único
  se rebaja a "dos executors de arquitectura distinta, una familia".
  Hallazgo adicional no anticipado por H1 pero coherente con el prior
  del proyecto (plan-en-prosa): en el executor más débil el fallo es
  además CONDUCTUAL (éxito degradado), no solo económico — la
  expectativa direccional "más débil = más margen de ΔR" queda medida
  y REFUTADA en su versión optimista.
Sin iteración, conforme al criterio 3.
```
