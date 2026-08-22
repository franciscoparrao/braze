# Evaluación de FreeToken en Nitro (2026-08-22)

Objetivo: ver si FreeToken permite servir el tier MoE grande
(Ornith-1.5-35B-A3B, gemma-4-26B-A4B) que hoy no cabe en Nitro, y si
podría adelantar experimentos sin esperar la ampliación de RAM.

**Resultado: DESBLOQUEADO** (CUDA 13 instalado vía pip dentro del venv,
sin sudo y sin tocar el sistema). El bench completo corrió en modo
`hybrid`, y su veredicto sobre este hardware es más informativo que
cualquier estimación: **el cuello de botella de Nitro es el PCIe, no
la RAM** — ver § Medición con PCIe gather.

## Lo que sí funcionó

- Instalación limpia en venv aislado (`uv`, Python 3.12 — el sistema
  tiene 3.14, demasiado nuevo para el stack ML): torch 2.11,
  transformers 5.15, triton, uvicorn. `INSTALL-EXIT-0`.
- CLI operativo (`ft`): `serve`, `shell`, `ctl`, `daemon`, `launch`,
  `checkpoint`, `bench`.
- El micro-benchmark `ft bench bw` corrió y midió el hardware real.

## El dato valioso: ancho de banda real de Nitro

`ft bench bw` (CPU-MoE, por formato de cuantización):

| formato | ancho de banda |
|---|---|
| bf16 | **58,2 GB/s** |
| nvfp4 | 33,9 GB/s |
| ds_fp4 | 22,9 GB/s |
| mxfp4 | 15,8 GB/s |

**58,2 GB/s en bf16 es consistente con DDR5-5200 dual-channel**
(~83 GB/s teóricos × ~70% de eficiencia real). Esto **confirma de
forma independiente el escepticismo** anotado en
`docs/nota-llmfit-2026-08-21.md`: llmfit reporta *"RAM Bandwidth
~125 GB/s (measured)"*, que excede el máximo teórico del bus y por
tanto infla sus estimaciones de tok/s. Dos herramientas, dos
mediciones, y la que cuadra con la física es la de FreeToken.

Corolario práctico: las proyecciones de velocidad para la compra de
RAM deben usarse sobre ~58 GB/s efectivos, no 125.

Segundo dato, no anticipado: **mxfp4 es el formato más lento en CPU**
(15,8 GB/s, 3,7× por debajo de bf16). Nuestro `gpt-oss-20b-MXFP4.gguf`
—el modelo estrella del proyecto— usa justamente ese formato. En el
camino CPU eso es un impuesto de descompresión real, y explicaría
parte de la lentitud que atribuimos a otras causas. Anotado como
hipótesis, no medido en nuestro banco.

## El bloqueante

`ft bench bw` reporta, para todos los formatos:

> *pcie gather unavailable (nvcc 12.4 would build kernels linking
> libcudart.so.12, but torch 2.11.0+cu130 ships CUDA 13.0
> (libcudart.so.13))*

Nitro tiene **CUDA 12.4**; FreeToken viene con torch construido contra
**CUDA 13.0**. Sin PCIe gather, el motor cae a modo *offload* puro —
que es esencialmente lo que ya hacemos con llama.cpp, **sin su ventaja
principal** (co-ejecución CPU-GPU adaptativa al ancho de banda).

Intento de arreglo y por qué se revirtió: bajar a `torch 2.6.0+cu124`
alineó CUDA, pero FreeToken usa API de torch ≥2.11 y su bench se cayó
(`measure_pcie_bw`). Se restauró el venv a torch 2.13+cu130, que queda
funcional en modo offload.

## Opciones (decisión del autor)

1. **Instalar CUDA 13.x toolkit en Nitro** (~4 GB, cambia el sistema).
   Es la vía correcta y desbloquea la evaluación real. Riesgo: puede
   afectar otras builds del nodo — notablemente la receta CUDA del
   LocalBackend, que se compiló contra 12.4.
2. **`FREETOKEN_ALLOW_CUDA_MISMATCH=1`**: forzar. El propio mensaje lo
   ofrece, pero enlaza kernels contra una runtime distinta; riesgo de
   fallos silenciosos, que es justo la clase de error que este
   proyecto persigue. No recomendado para medir.
3. **Aceptar modo offload puro**: mediría algo que ya tenemos, sin la
   ventaja del sistema. Poco valor.
4. **Dejarlo hasta la ampliación de RAM**: con 32-64 GB, el tier MoE
   grande corre en llama.cpp/Ollama sin runtime nuevo ni riesgo de
   comparabilidad.

**Recomendación**: opción 4, con 1 como alternativa si aparece un
modelo que solo FreeToken pueda servir. El costo de (1) no es la
descarga sino el riesgo de romper la toolchain CUDA del LocalBackend,
que es infraestructura de experimentos ya validada.

## Estado dejado en Nitro

- `~/freetoken-venv/` — venv aislado, funcional en modo offload.
  Desinstalar es `rm -rf ~/freetoken-venv` (no toca el sistema).
- `~/.local/bin/uv` — instalado, útil de todos modos.
- `~/.cache/freetoken/benchbw.json` — resultados del bench.
- Disco: 97 GB libres (se consumieron ~6 GB).
- **No se descargó ningún modelo**: el formato nativo es FTW/
  safetensors (`ft checkpoint` convierte desde HF), no los GGUF que
  ya tenemos, así que probar el serving exigía bajar 13-20 GB. No se
  hizo por el bloqueante anterior.

---

# ACTUALIZACIÓN (2026-08-22, tarde): desbloqueado y medido

## Cómo se resolvió el CUDA mismatch — sin sudo, sin fork

No hizo falta instalar CUDA 13 en el sistema ni preservar/forkear la
configuración del LocalBackend (preocupación válida del autor).
NVIDIA publica el compilador como paquete pip: `nvidia-cuda-nvcc`
(13.3.73) instala `nvcc` **dentro del venv**. Verificado:

```
nvcc del venv:     cuda_13.3.r13.3
nvcc del sistema:  cuda_12.4.r12.4   ← INTACTO
```

La toolchain del LocalBackend sigue exactamente como estaba. Un
`rm -rf ~/freetoken-venv` revierte todo.

Nota de proceso (error propio, documentado): el primer venv quedó
inservible por un flip-flop de versiones de torch (2.11 → 2.6 → 2.13)
que dejó las extensiones compiladas contra un ABI muerto
(`INTERNAL ASSERT FAILED ... no interpreter set`). Se recreó limpio
con la secuencia correcta: venv → `freetoken[accel]` (trae su torch) →
`nvidia-cuda-nvcc`. **Lección**: no manipular la versión de torch de un
stack que trae extensiones compiladas; recrear el entorno es más
barato que repararlo.

## Medición con PCIe gather activo (`ft bench bw`, modo hybrid)

| formato | CPU-MoE | PCIe-gather | ratio CPU/PCIe | hybrid usa PCIe para |
|---|---|---|---|---|
| bf16 | 54,2 GB/s | **5,6 GB/s** | 9,67× | 9,1% de los misses |
| nvfp4 | 36,6 GB/s | 5,7 GB/s | 6,40× | 14,9% |
| ds_fp4 | 19,6 GB/s | 5,6 GB/s | 3,49× | 21,4% |
| mxfp4 | 14,3 GB/s | 5,5 GB/s | 2,61× | 27,9% |

**El hallazgo: el PCIe de Nitro mide 5,6 GB/s** — un orden de magnitud
por debajo del cómputo CPU-MoE (54 GB/s en bf16). Para un laptop eso
sugiere un enlace estrecho (PCIe x4, o x8 en generación baja); un
PCIe 4.0 ×16 daría 25-30 GB/s.

Consecuencia directa sobre la hipótesis que motivó esta evaluación:
**la ventaja principal de FreeToken —mantener expertos en VRAM y
traer los que falten por PCIe— tiene poco margen en este hardware.**
El propio planificador lo reconoce: decide enrutar por PCIe apenas el
**9,1%** de los misses en bf16, porque para el 91% restante es más
rápido computar en CPU. No es un defecto del sistema; es su
adaptatividad funcionando y diciéndonos que aquí no hay mucho que
ganar.

Estimación revisada del speedup esperable en Nitro: **modesto** (del
orden del 9-28% de los accesos que fallan al caché, no un factor
2-3×). La proyección optimista que motivó esta evaluación —"expertos
calientes en VRAM a 350 GB/s"— **no se sostiene**, porque el costo no
es leer de VRAM sino *llegar* a la VRAM por un bus de 5,6 GB/s.

Segundo dato, ya anotado arriba y ahora confirmado en modo hybrid:
**mxfp4 es el peor formato para el camino CPU** (14,3 GB/s, 3,8×
debajo de bf16) — y es el de `gpt-oss-20b-MXFP4`, el modelo estrella
del proyecto. Es el único caso donde el hybrid aporta más (27,9% de
los misses), justamente porque el camino CPU es tan lento que el PCIe
compite. Sigue siendo hipótesis para nuestro banco, no medición
nuestra.

## Qué falta para un veredicto definitivo

El bench mide techos de componentes, no throughput end-to-end. Un
juicio final exigiría servir un modelo MoE en safetensors/FTW y
compararlo contra nuestro baseline de llama.cpp con el mismo modelo
(gpt-oss:20b: 57/57 y ~41 s/tarea vía LocalBackend). Costo: descarga
de 13-20 GB + conversión con `ft checkpoint`. Viable (97 GB libres),
pero **el bench ya bajó mucho la expectativa de ganancia**, así que la
relación costo/beneficio de esa medición es peor de lo que era esta
mañana.

**Recomendación actualizada**: no seguir hasta la medición end-to-end
por ahora. El cuello de botella de Nitro es el PCIe, y eso no lo
arregla ni FreeToken ni más RAM — solo otro equipo. Si el autor
evalúa hardware nuevo, **el ancho de banda PCIe entre CPU y GPU pasa
a ser un criterio de compra tan importante como la VRAM**, y este
número (5,6 GB/s) es el punto de comparación.
