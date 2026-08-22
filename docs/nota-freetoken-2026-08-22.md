# Nota: FreeToken (arXiv 2608.16157 + FlashML-org/FreeToken)

Fecha: 2026-08-22. **Yang, Fan, Pan, Xi, Wang, Sun, Keutzer, Han,
Zaharia, Xu, Stoica** — Berkeley + MIT. Apache 2.0, ~1,3k estrellas.
Clave: `yang2026freetoken`. Autoría de primer nivel en sistemas
(Stoica: Spark/Ray/vLLM; Zaharia: Spark/Databricks; Han: compresión).

## Qué es

Motor de serving **MoE edge-native**: trata la máquina personal no
como "una GPU chica" sino como plataforma elástica unificada.
Co-diseña layout y carga del modelo, **residencia de expertos**,
ejecución CPU-GPU adaptativa al ancho de banda, **reuso de estado
agéntico** y gestión de memoria en runtime. En vez de fijar una
estrategia de offloading, mapea cómputo y estado sobre los recursos
que realmente hay. Reclama: **35B en un laptop**, 284B en un desktop
gamer, GLM-5.2 (753B) en una workstation de una sola GPU; >20 modelos
MoE; API compatible OpenAI/Anthropic.

## Por qué importa para Nitro (directamente)

Nuestro lineup relevante es **todo MoE**: gpt-oss:20b, gemma-4-26B-A4B,
Ornith-1.5-35B-A3B. FreeToken está diseñado exactamente para ese caso
y para nuestro perfil de hardware (declara soporte desde laptop con
GPU de 8 GB; Nitro tiene 6 GB VRAM + 14 GB RAM). Su *global LRU expert
caching* y la **reasignación dinámica de VRAM entre caché de expertos
y KV** atacan de frente la clase de falla que nos costó dos
experimentos esta semana.

**Vía de integración sin código nuevo**: expone API compatible
OpenAI/Anthropic, y braze ya tiene backends para ambas. Es la primera
alternativa seria a Ollama/LocalBackend para el tier grande **sin
esperar la RAM**.

Caveats antes de entusiasmarse: el README no publica benchmarks de
calidad-vs-velocidad (el paper tiene números, pero de throughput); no
menciona integración llama.cpp/Ollama; y cambiar de runtime rompe
comparabilidad con nuestros baselines (habría que re-establecerlos,
igual que discutimos para un cambio de hardware). Aplicaría el rito de
adopción completo.

## El hallazgo que toca al Paper 2 (importante)

Entre sus técnicas: **"semantic anchor checkpoints for KV caches
enabling agentic context edits without full recomputation"**. Eso
apunta a un hueco de nuestro propio manuscrito.

El Paper 2 §3 afirma: *"agentic loops re-send the (growing)
conversation to the model each round, so a memory section of `c`
tokens does not cost `c` tokens per task but `c × R` tokens"*. El
manuscrito **no menciona caching, prefix caching ni prefill en ninguna
parte** (verificado: 0 ocurrencias). Un revisor de sistemas objetará,
con razón, que con prefix caching —que llama.cpp, Ollama, vLLM y las
APIs de Anthropic/OpenAI implementan— el prefijo estable se procesa
**una vez** y su KV queda cacheado: `c × R` sería entonces
contabilidad de tokens, no recómputo.

**La objeción no invalida el resultado, pero el paper debe
anticiparla** — y lo bueno es que nuestros propios datos la responden:

| tarea | net Δtokens | Δwall time |
|---|---|---|
| original | −304 | 57,4→40,6 s (Holm 0,011) |
| loop | **+1076** | 49,5→51,2 s (n.s., p=0,55) |
| move | **+1132** | 48,5→47,0 s (n.s., p=0,62) |

En las dos tareas frescas, **+1000 tokens netos NO se tradujeron en
wall time**. Esa es exactamente la firma de un prefijo cacheado: los
tokens se contabilizan, no se recomputan. Es decir, el costo real de
la memoria inyectada en este régimen es (i) **contable** —lo que se
factura en APIs, y lo que el proyecto midió—, (ii) de **ocupación de
KV**, que compite con el resto del contexto y en hardware ajustado es
material (los OOM del 20-ago lo demuestran), y (iii) **conductual**,
que es donde más duele: en la replicación con ornith:9b el playbook
alargó trayectorias y degradó el éxito.

Corrección propuesta al manuscrito: declarar el mecanismo con
precisión en §3 y reconocer el caching explícitamente, apoyándose en
la evidencia de wall time. Mejora el paper —anticipa la objeción en
vez de regalarla— y no toca ningún número.

## Acciones

1. Corregir §3 del Paper 2 (arriba). **Antes de someter.**
2. `yang2026freetoken` a la cola del bib del follow-up: es la
   referencia canónica para "el harness no controla la economía de
   tokens solo; el runtime también".
3. Backlog de infraestructura: evaluar FreeToken como runtime para el
   tier MoE grande, con rito de adopción y baselines nuevos. No
   sustituye la RAM, pero podría adelantar el 35B-MoE.
