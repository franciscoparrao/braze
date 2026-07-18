# fig1_curva.R — La curva harness-vs-escala (figura central del paper).
#
# Pass rate (IC 95% Wilson) vs escala del executor, una línea por brazo
# de harness. Fuente de datos: los JSONs crudos commiteados del sweep
# 2026-07-10 (1.520 corridas, binario e9b841e):
#   - docs/sweep-curva-multiescala-2026-07-10.qwen.json   (12 brazos qwen)
#   - docs/sweep-curva-multiescala-2026-07-10.partial-1b.json
#     (slice válido de llama3.2:1b; sus filas qwen muertas se excluyen)
# Análisis de referencia: docs/sweep-curva-multiescala-2026-07-10.md
#
# Línea de referencia horizontal `gemma4:e4b` solo (2026-07-13,
# docs/gemma4-e4b-solo-baseline-design.md): 87/95, criterio
# pre-registrado disparó "revisar framing" — el compuesto 1B+lead no es
# distinguible de lo que el propio lead saca sin ningún executor de 1B
# adjunto. Fuente: docs/sweep-gemma4-e4b-solo-2026-07-13.json.
#
# Correr desde la raíz del repo:  Rscript paper/R/fig1_curva.R

suppressPackageStartupMessages({
  library(dplyr)
  library(tidyr)
  library(purrr)
  library(ggplot2)
  library(jsonlite)
  library(ggrepel)
})
source("paper/R/theme_paper.R")
fontfam <- setup_paper_theme(journal = "elsevier")

# ---- Datos ----------------------------------------------------------------

leer_sweep <- function(path) {
  fromJSON(path)$results |>
    as_tibble() |>
    select(backend, passed)
}

# Runs perdidos por TRANSPORTE (el request nunca llegó al servidor, o el
# stream murió en vuelo): mismo criterio de docs/emse-r2-analysis-
# 2026-07-17.md § 4, aplicado acá tras docs/curve-transport-audit-
# 2026-07-18.md. Son filas MUERTAS, no corridas degradadas: el brazo
# 1B+plan+lead perdió 30/95 así (las 30 puntúan 0/30) y sin excluirlas
# la celda marca 61.1% en vez de 89.2%, invirtiendo la lectura. Los
# empty-response genuinos NO califican.
#
# Se excluye SOLO en esa celda, la única con contaminación estructural:
# las otras cuatro del sweep tienen 1-4 runs de transporte y su magnitud
# se disclosa en § Threats (máx +3.0pp, dentro de sus propios Wilson) —
# excluir 1-4 corridas de celdas con intervalos de ~10pp es contabilidad,
# no corrección.
CELDA_CONTAMINADA <-
  "ollama:llama3.2:1b+plan:ollama:gemma4:e4b+lead:ollama:gemma4:e4b"

leer_sweep_limpio <- function(path, solo_brazo) {
  r <- fromJSON(path)$results |> as_tibble()
  err <- ifelse(is.na(r$run_error), "", as.character(r$run_error))
  transporte <- !is.na(r$failure_cause) &
    r$failure_cause == "model_backend_error" &
    (r$wall_time_ms < 1000 |
       grepl("request to model backend failed|stream failed", err)) &
    r$backend %in% solo_brazo
  r[!transporte, ] |> select(backend, passed)
}

curva <- bind_rows(
  leer_sweep("docs/sweep-curva-multiescala-2026-07-10.qwen.json"),
  leer_sweep_limpio("docs/sweep-curva-multiescala-2026-07-10.partial-1b.json",
                    solo_brazo = CELDA_CONTAMINADA) |>
    filter(startsWith(backend, "ollama:llama3.2:1b"))
)

brazo_de <- function(b) {
  case_when(
    grepl("\\+plan", b) & grepl("\\+lead", b) ~ "plan+lead",
    grepl("\\+plan", b) ~ "+plan",
    grepl("\\+lead", b) ~ "+lead",
    .default = "baseline"
  )
}
executor_de <- function(b) sub("^ollama:([^+]+)\\+?.*$", "\\1", b)

# IC 95% de Wilson — el mismo del análisis de los sweeps.
wilson <- function(x, n, z = 1.96) {
  p <- x / n
  den <- 1 + z^2 / n
  centro <- (p + z^2 / (2 * n)) / den
  h <- z * sqrt(p * (1 - p) / n + z^2 / (4 * n^2)) / den
  tibble(lo = 100 * (centro - h), hi = 100 * (centro + h))
}

datos <- curva |>
  mutate(
    executor = executor_de(backend),
    brazo = brazo_de(backend)
  ) |>
  summarise(pases = sum(passed), n = n(), .by = c(executor, brazo)) |>
  mutate(
    pass_rate = 100 * pases / n,
    wilson(pases, n),
    executor = factor(executor,
      levels = c("llama3.2:1b", "qwen2.5:3b", "qwen2.5:7b", "qwen3.5-coder"),
      labels = c("Llama 3.2\n1B", "Qwen 2.5\n3B", "Qwen 2.5\n7B", "Qwen 3.5\nCoder")
    ),
    brazo = factor(brazo, levels = c("baseline", "+plan", "+lead", "plan+lead"),
                   labels = c("baseline", "+planner", "+lead", "+planner+lead"))
  )

# 15 celdas intactas a n=95; la contaminada queda en 65 tras excluir sus
# 30 filas muertas por transporte (ver comentario de CELDA_CONTAMINADA).
stopifnot(nrow(datos) == 16, sum(datos$n == 95) == 15, sum(datos$n == 65) == 1)

# `gemma4:e4b` solo — referencia horizontal, no forma parte del eje de
# escala de executors (no es una fila de la curva, es el techo del
# propio modelo lead). Pooled n=285: sweep original (95, ec61f5e) +
# power sweep pre-declarado (190, ec61f5e; homogeneidad Fisher p=1.00,
# docs/emse-r2-analysis-2026-07-17.md).
gemma_solo <- bind_rows(
  leer_sweep("docs/sweep-gemma4-e4b-solo-2026-07-13.json"),
  leer_sweep("docs/sweep-gemma4-e4b-solo-power-2026-07-13.json")
) |>
  summarise(pases = sum(passed), n = n()) |>
  mutate(pass_rate = 100 * pases / n, wilson(pases, n))
stopifnot(nrow(gemma_solo) == 1, gemma_solo$n == 285)

# ---- Figura ---------------------------------------------------------------

# Semántica de color (Wong, colorblind-safe): baseline negro (referencia),
# +lead azul (la palanca que gana), +planner bermellón (la que daña),
# combinado morado.
pal_brazos <- c(
  "baseline"      = "#000000",
  "+planner"      = "#D55E00",
  "+lead"         = "#0072B2",
  "+planner+lead" = "#CC79A7"
)

dodge <- position_dodge(width = 0.18)

p <- ggplot(datos, aes(x = executor, y = pass_rate,
                       color = brazo, group = brazo)) +
  # Referencia: gemma4:e4b solo (el propio lead, sin ningún executor de
  # 1B adjunto) — banda del IC 95% Wilson + línea punteada al punto
  # estimado. Dibujada primero para quedar detrás de las series de datos.
  annotate("rect", xmin = -Inf, xmax = Inf,
           ymin = gemma_solo$lo, ymax = gemma_solo$hi,
           fill = "grey50", alpha = 0.12) +
  geom_hline(yintercept = gemma_solo$pass_rate,
             linetype = "22", linewidth = 0.4, color = "grey35") +
  annotate("text", x = 0.8, y = gemma_solo$pass_rate + 2.6,
           label = "gemma4:e4b solo (lead alone)",
           family = fontfam, size = 2.35, fontface = "italic",
           color = "grey35", hjust = 0) +
  geom_errorbar(aes(ymin = lo, ymax = hi),
                width = 0.12, linewidth = 0.35, position = dodge) +
  geom_line(linewidth = 0.55, position = dodge) +
  geom_point(size = 1.6, position = dodge) +
  # Direct labels al final de cada línea — sin leyenda (regla de oro).
  geom_text_repel(
    data = datos |> filter(as.integer(executor) == 4),
    aes(label = brazo),
    family = fontfam, size = 2.65, fontface = "bold",
    nudge_x = 0.32, direction = "y", hjust = 0,
    segment.size = 0.25, segment.color = "grey60",
    min.segment.length = 0.1, box.padding = 0.12,
    xlim = c(4.25, 5.15), seed = 42
  ) +
  scale_color_manual(values = pal_brazos, guide = "none") +
  scale_y_continuous(
    limits = c(0, 100),
    breaks = seq(0, 100, 25),
    expand = expansion(mult = c(0.01, 0.02))
  ) +
  scale_x_discrete(expand = expansion(add = c(0.25, 1.15))) +
  labs(
    x = "Executor model (increasing scale →)",
    y = "Pass rate (%)"
  ) +
  theme(plot.margin = margin(4, 2, 4, 4, "pt"))

save_paper(p, "paper/figs/fig1_curva.pdf", width_cm = 18.0, height_cm = 10.0)

# SessionInfo para reproducibilidad (al log, no a la figura).
writeLines(capture.output(sessionInfo()), "paper/figs/fig1_curva.sessioninfo.txt")
