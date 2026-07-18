# fig2_rescate.R — El rescate del planner: efecto de cada variante de
# entrega del plan sobre su baseline within-sweep.
#
# La degeneración del planner original (plan como texto de ASSISTANT) y
# su corrección (render user-role; entrega como task list tipada) — la
# figura muestra Δ pass rate (pp) de cada variante contra el baseline de
# SU PROPIO sweep, porque los brazos vienen de dos sweeps distintos y
# los baselines difieren entre sesiones (coder 98% vs 86%): comparar
# deltas within-sweep es lo honesto; comparar niveles cross-sweep no.
#
# Fuentes (todas commiteadas):
#   - planner viejo (prosa-assistant): docs/sweep-curva-multiescala-2026-07-10.qwen.json
#   - variantes nuevas: docs/sweep-planner-ab-2026-07-11.json
#   - brazo 3b+task-list: docs/sweep-planner-ab-3b-tasklist-rerun-2026-07-11.json
#     (la corrida original se contaminó con 58 fallos transitorios de red;
#     ver docs/sweep-planner-ab-2026-07-11.md § nota)
#
# Exclusión de fallos de transporte (2026-07-17, R2 EMSE): el mismo
# evento de red que contaminó el brazo 3B task-list también alcanzó los
# brazos coder del sweep AB (10/2/8 fallos en baseline/user/task-list).
# Para TODOS los brazos de ese sweep (y del re-run) se excluyen del
# numerador y denominador las corridas perdidas por transporte
# (model_backend_error con wall<1s, o stream/request fallido); los
# empty-response genuinos NO se excluyen. El sweep de la curva se deja
# crudo (sin evento documentado). Detalle y números raw vs corregidos:
# docs/emse-r2-analysis-2026-07-17.md § 4.
#
# IC 95% del delta: Newcombe/MOVER sobre intervalos de Wilson
# (proporciones independientes — corridas distintas, no pareadas).
#
# Correr desde la raíz del repo:  Rscript paper/R/fig2_rescate.R

suppressPackageStartupMessages({
  library(dplyr)
  library(tidyr)
  library(ggplot2)
  library(jsonlite)
})
source("paper/R/theme_paper.R")
fontfam <- setup_paper_theme(journal = "elsevier")

# ---- Datos ----------------------------------------------------------------

conteo <- function(path, backend_exacto, excluir_transporte = FALSE) {
  r <- fromJSON(path)$results
  filas <- r[r$backend == backend_exacto, ]
  stopifnot(nrow(filas) == 95)
  if (excluir_transporte) {
    err <- ifelse(is.na(filas$run_error), "", as.character(filas$run_error))
    transporte <- !is.na(filas$failure_cause) &
      filas$failure_cause == "model_backend_error" &
      (filas$wall_time_ms < 1000 |
         grepl("stream|request to model backend failed", err))
    filas <- filas[!transporte, ]
  }
  c(pases = sum(filas$passed), n = nrow(filas))
}

CURVA <- "docs/sweep-curva-multiescala-2026-07-10.qwen.json"
AB    <- "docs/sweep-planner-ab-2026-07-11.json"
RERUN <- "docs/sweep-planner-ab-3b-tasklist-rerun-2026-07-11.json"
L <- "+plan:ollama:gemma4:e4b"

celdas <- tribble(
  ~executor, ~variante, ~archivo, ~backend, ~archivo_base, ~backend_base,
  # planner viejo (curva): prosa como assistant
  "qwen2.5:3b", "assistant", CURVA, paste0("ollama:qwen2.5:3b", L), CURVA, "ollama:qwen2.5:3b",
  "qwen3.5-coder", "assistant", CURVA, paste0("ollama:qwen3.5-coder", L), CURVA, "ollama:qwen3.5-coder",
  # iteración: prosa como user
  "qwen2.5:3b", "user", AB, paste0("ollama:qwen2.5:3b", L), AB, "ollama:qwen2.5:3b",
  "qwen3.5-coder", "user", AB, paste0("ollama:qwen3.5-coder", L), AB, "ollama:qwen3.5-coder",
  # iteración: plan → task list tipada
  "qwen2.5:3b", "tasks", RERUN, paste0("ollama:qwen2.5:3b", L, "+ablate:task-list"), AB, "ollama:qwen2.5:3b",
  "qwen3.5-coder", "tasks", AB, paste0("ollama:qwen3.5-coder", L, "+ablate:task-list"), AB, "ollama:qwen3.5-coder"
)

wilson_lohi <- function(x, n, z = 1.96) {
  p <- x / n
  den <- 1 + z^2 / n
  centro <- (p + z^2 / (2 * n)) / den
  h <- z * sqrt(p * (1 - p) / n + z^2 / (4 * n^2)) / den
  c(lo = centro - h, hi = centro + h)
}

datos <- celdas |>
  rowwise() |>
  mutate(
    # Exclusión de transporte solo en el sweep AB y su re-run (evento de
    # red documentado); la curva se deja cruda.
    var_c = list(conteo(archivo, backend,
                        excluir_transporte = archivo %in% c(AB, RERUN))),
    base_c = list(conteo(archivo_base, backend_base,
                         excluir_transporte = archivo_base %in% c(AB, RERUN))),
    p_var = var_c[["pases"]] / var_c[["n"]],
    p_base = base_c[["pases"]] / base_c[["n"]],
    delta = 100 * (p_var - p_base),
    w_var = list(wilson_lohi(var_c[["pases"]], var_c[["n"]])),
    w_base = list(wilson_lohi(base_c[["pases"]], base_c[["n"]])),
    # Newcombe/MOVER para p_var - p_base, independientes.
    delta_lo = 100 * ((p_var - p_base) -
      sqrt((p_var - w_var[["lo"]])^2 + (w_base[["hi"]] - p_base)^2)),
    delta_hi = 100 * ((p_var - p_base) +
      sqrt((w_var[["hi"]] - p_var)^2 + (p_base - w_base[["lo"]])^2))
  ) |>
  ungroup() |>
  mutate(
    variante = factor(variante,
      levels = c("tasks", "user", "assistant"),
      labels = c("plan → typed task list",
                 "prose plan, user role",
                 "prose plan, assistant role\n(original)")
    ),
    executor = factor(executor,
      levels = c("qwen2.5:3b", "qwen3.5-coder"),
      labels = c("Qwen 2.5 3B", "Qwen 3.5 Coder")
    )
  )

stopifnot(nrow(datos) == 6)

# ---- Figura ---------------------------------------------------------------

# Semántica de color (Wong, colorblind-safe): esta figura colorea por
# IDENTIDAD DE MODELO, no por configuración de harness (fig1/fig3 colorean
# por brazo/config) -- eje semántico distinto, así que se evita a propósito
# reutilizar #0072B2 (azul = "+lead" en fig1, "deferred (search_tools)" en
# fig3) para no implicar que este azul significa lo mismo aquí.
pal_exec <- c("Qwen 2.5 3B" = "#E69F00", "Qwen 3.5 Coder" = "#009E73")
dodge <- position_dodge(width = 0.45)

p <- ggplot(datos, aes(x = delta, y = variante, color = executor)) +
  annotate("rect", xmin = -Inf, xmax = 0, ymin = -Inf, ymax = Inf,
           fill = "grey92", alpha = 0.55) +
  geom_vline(xintercept = 0, linewidth = 0.4, color = "grey30") +
  geom_errorbar(aes(xmin = delta_lo, xmax = delta_hi),
                orientation = "y", width = 0.14, linewidth = 0.4,
                position = dodge) +
  geom_point(size = 2.1, position = dodge) +
  annotate("text", x = -27, y = 3.42, label = "harms",
           family = fontfam, size = 2.6, color = "grey40",
           fontface = "italic", hjust = 0.5) +
  annotate("text", x = 13, y = 3.42, label = "helps",
           family = fontfam, size = 2.6, color = "grey40",
           fontface = "italic", hjust = 0.5) +
  scale_color_manual(values = pal_exec, name = NULL) +
  scale_x_continuous(
    limits = c(-60, 25),
    breaks = seq(-60, 20, 20),
    expand = expansion(mult = c(0.01, 0.02))
  ) +
  labs(
    x = "\u0394 pass rate vs. baseline (pp)",
    y = NULL
  ) +
  theme(
    legend.position = "top",
    legend.margin = margin(0, 0, 2, 0, "pt"),
    axis.text.y = element_text(hjust = 1, lineheight = 0.95),
    plot.margin = margin(4, 6, 4, 4, "pt")
  )

save_paper(p, "paper/figs/fig2_rescate.pdf", width_cm = 8.8, height_cm = 6.2)

writeLines(capture.output(sessionInfo()), "paper/figs/fig2_rescate.sessioninfo.txt")
