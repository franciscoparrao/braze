# Nota: llmfit — el pre-flight de memoria que nos faltó (y una feature que justifica)

Fecha: 2026-08-21. Fuente: github.com/AlexsJones/llmfit (Rust, MIT,
~33k estrellas). Instalado en Nitro (`cargo install llmfit`, v1.1.10).

## Qué hace

Estima qué modelos corren en TU hardware: detecta CPU/RAM/GPU, mide
ancho de banda, y puntúa modelos en fit/velocidad/calidad/contexto con
un modelo de ancho de banda de memoria + benchmarks de comunidad.
Integra Ollama/llama.cpp/MLX. Subcomandos: `fit` (ranking),
`info <modelo>` (requisitos y base del cálculo), `bench` (medición
real), `recommend --json`.

## El dato que duele: habría predicho los dos incidentes del 20-ago

Su detección de Nitro: i5-13420H, **14,57 GB RAM (11,12 disponibles)**,
RTX 3050 **6 GB**, backend CUDA. Y para el modelo del sweep KV-quant:

```
openai/gpt-oss-20b →  Min RAM: 12.0 GB (CPU inference)
                      Recommended RAM: 20.0 GB
```

El **mínimo (12 GB) ya excede la RAM disponible del nodo (11,12 GB)** y
el recomendado (20 GB) supera el total instalado. Corrimos ese modelo
con KV f16 a 32k de contexto — que suma bastante más — durante ~50 h.
Los OOM no fueron mala suerte: eran predecibles con un comando de tres
segundos. Lo mismo aplica al 2×2 (Ornith-1.5, 6,6 GB + KV 32k ≈ 12,9 GB
de VM observados).

## Escepticismo justo (lo que no me creo del todo)

- **"RAM Bandwidth: ~125 GB/s (measured)"**: DDR5-5200 dual-channel da
  ~83 GB/s TEÓRICOS. Medir 125 GB/s sugiere que el benchmark toca
  caché o usa un patrón no representativo del decode. Si el BW está
  inflado, los tok/s estimados también.
- **Estima a ctx ≤ 8192 y no modela prefill/TTFT** (lo declara).
  Nuestro régimen es 32k: ahí el KV pesa mucho más, así que sus cifras
  de memoria son un PISO, no el consumo real de nuestros sweeps.
- `Context Length: 4194304` para gpt-oss-20b huele a metadata mal
  parseada.
- Su score da `Fit: 100` mientras el desglose dice que no cabe: el
  score asume GPU; hay que leer "Resource Requirements", no el número
  grande.

A su favor, y es notable: **declara su método y su banda de error**
(±30%), dice qué NO estima, y pide reportar hardware ausente de su
tabla. Es la clase de honestidad metodológica que este proyecto
valora — el opuesto exacto del post de Qwen-27B-en-8GB.

## Acción concreta que esto justifica

**Pre-flight de memoria en `braze-bench`** (palanca de
infraestructura, no de harness): antes de lanzar un sweep, estimar
`pesos + KV(ctx, tipo) + overhead` contra la RAM/VRAM disponible y
**advertir o abortar** si no cabe, en vez de descubrirlo por OOM a las
6 horas. Los dos incidentes del 20-ago son la justificación empírica;
llmfit es la prueba de que el cálculo es tratable (y su fórmula, MIT y
en Rust, es referencia directa — mismo lenguaje del workspace).
Alcance mínimo: un `braze-bench doctor` o un chequeo en el arranque
del runner con kill-switch.

## Validación de la decisión de RAM (pendiente del autor)

Con los requisitos que llmfit reporta, 32 GB alcanzan holgado para el
lineup actual (gpt-oss-20b recomienda 20 GB; ornith 9B ~13 GB con KV
32k) y **64 GB** son los que dan margen para el tier que interesa
después (Ornith-1.5-35B-MoE, gemma-4-26B-A4B) más dos modelos
residentes para brazos lead/worker. La recomendación 2×32 se sostiene
con datos de terceros, no solo con mi aritmética.
