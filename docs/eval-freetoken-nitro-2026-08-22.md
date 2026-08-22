# Evaluación de FreeToken en Nitro (2026-08-22)

Objetivo: ver si FreeToken permite servir el tier MoE grande
(Ornith-1.5-35B-A3B, gemma-4-26B-A4B) que hoy no cabe en Nitro, y si
podría adelantar experimentos sin esperar la ampliación de RAM.

**Resultado: BLOQUEADO por un mismatch de CUDA. No se llegó a servir
ningún modelo.** Pero el ejercicio dejó un dato de hardware valioso.

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
