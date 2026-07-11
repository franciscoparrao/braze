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

curva <- bind_rows(
  leer_sweep("docs/sweep-curva-multiescala-2026-07-10.qwen.json"),
  leer_sweep("docs/sweep-curva-multiescala-2026-07-10.partial-1b.json") |>
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

stopifnot(nrow(datos) == 16, all(datos$n == 95))

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
