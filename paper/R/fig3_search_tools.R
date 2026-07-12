# fig3_search_tools.R — Deferral de herramientas en dos niveles
# (search_tools): 5-6× menos tokens de prompt, con costo de correctness
# real a esta escala de modelo (-24pp en 3b, -13pp en 7b, ICs disjuntos).
#
# Datos: docs/sweep-search-tools-ab-n15-2026-07-12.json (360 corridas —
# 2 brazos × {3b, 7b} × 6 tareas × 15 reps sobre tool-search.toml: 200
# tools de ruido sintético; binario 57db13f con el harness endurecido de
# la auditoría v7: gate J-9, assert estricto J-7, budget justo J-17).
# Análisis y diagnóstico de fragilidad composicional (las sondas de
# noisy_no_tool / noisy_multi_step):
# docs/sweep-search-tools-ab-n15-2026-07-12.md. Las dos corridas previas
# (n=30: docs/sweep-search-tools-ab-2026-07-11.md y el re-run postgate)
# quedan como análisis de sensibilidad — el "mismo pass rate" original
# era artefacto de n=30 + assert laxo pre-J-7.
#
# Panel (a): tokens de input POR CORRIDA (puntos crudos + mediana) — se
# muestran los datos, no solo medias (regla anti-dynamite). Panel (b):
# pass rate con IC 95% Wilson.
#
# Correr desde la raíz del repo:  Rscript paper/R/fig3_search_tools.R

suppressPackageStartupMessages({
  library(dplyr)
  library(ggplot2)
  library(jsonlite)
  library(patchwork)
})
source("paper/R/theme_paper.R")
fontfam <- setup_paper_theme(journal = "elsevier")

# ---- Datos ----------------------------------------------------------------

res <- fromJSON("docs/sweep-search-tools-ab-n15-2026-07-12.json")$results |>
  as_tibble() |>
  transmute(
    executor = factor(
      sub("^ollama:([^+]+)\\+?.*$", "\\1", backend),
      levels = c("qwen2.5:3b", "qwen2.5:7b"),
      labels = c("Qwen 2.5 3B", "Qwen 2.5 7B")
    ),
    brazo = factor(
      ifelse(grepl("threshold", backend), "off", "on"),
      levels = c("on", "off"),
      labels = c("deferred (search_tools)", "full inventory (206 tools)")
    ),
    input_tokens,
    passed
  )
stopifnot(nrow(res) == 360)

wilson <- function(x, n, z = 1.96) {
  p <- x / n
  den <- 1 + z^2 / n
  centro <- (p + z^2 / (2 * n)) / den
  h <- z * sqrt(p * (1 - p) / n + z^2 / (4 * n^2)) / den
  tibble(lo = 100 * (centro - h), hi = 100 * (centro + h))
}

pass <- res |>
  summarise(pases = sum(passed), n = n(), .by = c(executor, brazo)) |>
  mutate(pass_rate = 100 * pases / n, wilson(pases, n))

pal_brazos <- c(
  "deferred (search_tools)"    = "#0072B2",
  "full inventory (206 tools)" = "grey45"
)
dodge <- position_dodge(width = 0.55)

# ---- Panel (a): tokens por corrida ----------------------------------------

pa <- ggplot(res, aes(x = executor, y = input_tokens / 1000, color = brazo)) +
  geom_point(
    position = position_jitterdodge(jitter.width = 0.13, dodge.width = 0.55,
                                    seed = 42),
    size = 0.8, alpha = 0.55, shape = 16
  ) +
  # Mediana como barra negra (color fijo: sobre los puntos del mismo
  # color de brazo sería invisible); el grupo mantiene el dodge.
  stat_summary(
    aes(group = brazo),
    fun = median, geom = "crossbar", color = "black",
    width = 0.3, linewidth = 0.45, position = dodge,
    show.legend = FALSE
  ) +
  scale_color_manual(values = pal_brazos, name = NULL) +
  scale_y_continuous(
    limits = c(0, NA),
    expand = expansion(mult = c(0.01, 0.06))
  ) +
  labs(x = NULL, y = "Input tokens per run (×1000)") +
  guides(color = guide_legend(override.aes = list(size = 2, alpha = 1))) +
  theme(legend.position = "top",
        legend.margin = margin(0, 0, 2, 0, "pt"),
        # El eje x lo lleva el panel (b), compartido abajo.
        axis.text.x = element_blank(),
        axis.ticks.x = element_blank())

# ---- Panel (b): pass rate --------------------------------------------------

pb <- ggplot(pass, aes(x = executor, y = pass_rate, color = brazo)) +
  geom_errorbar(aes(ymin = lo, ymax = hi),
                width = 0.16, linewidth = 0.4, position = dodge) +
  geom_point(size = 2.0, position = dodge) +
  scale_color_manual(values = pal_brazos, guide = "none") +
  scale_y_continuous(
    limits = c(0, 100),
    breaks = seq(0, 100, 25),
    expand = expansion(mult = c(0.01, 0.02))
  ) +
  labs(x = NULL, y = "Pass rate (%)")

# ---- Composición -----------------------------------------------------------

p <- (pa / pb) +
  plot_layout(heights = c(1.15, 1)) +
  plot_annotation(tag_levels = "a", tag_suffix = ")") &
  theme(plot.tag = element_text(face = "bold", size = 10, family = fontfam))

save_paper(p, "paper/figs/fig3_search_tools.pdf",
           width_cm = 8.8, height_cm = 10.5)

writeLines(capture.output(sessionInfo()),
           "paper/figs/fig3_search_tools.sessioninfo.txt")
