# fig1_frontera.R — La frontera de amortización (figura central del Paper 2).
#
# Plano (ΔR = rondas ahorradas por el playbook, ΔT = diferencia de tokens
# de entrada por ronda, playbook − none): la curva de equilibrio de la
# ec. (1) del paper,
#   ΔT* = ΔR · T_base / R_mem
# separa la región donde la memoria amortiza (bajo la curva) de donde
# cuesta tokens netos (sobre ella).
#
# Revisión EMSE PROMPT-SE (B13, 2026-11-13; fuente única 2026-11-14):
# los puntos e intervalos NO se calculan aquí. Los escribe
#   python3 paper2/analysis/reanalysis.py --fig1-csv paper2/analysis/fig1-points.csv
# con las mismas funciones que generan las tablas del paper (T como razón
# de medias, con la que la Ec. 1 es exacta; IC de ΔR de Welch; IC de ΔT
# bootstrap percentil, 10.000 réplicas, semilla 0). El stopifnot de abajo
# verifica que la identidad cierre en cada punto.
#
# La frontera de cada ejecutor se dibuja como BANDA: cada tarea tiene su
# propio T_base y R_mem; la banda cubre el rango de sus tres curvas. Solo
# para ΔR ≥ 0: con ΔR < 0 y ΔT > 0 la memoria cuesta tokens siempre.
#
# Correr desde la raíz del repo:  Rscript paper2/R/fig1_frontera.R

suppressPackageStartupMessages({
  library(dplyr)
  library(tidyr)
  library(ggplot2)
  library(ggrepel)
})
source("paper/R/theme_paper.R")
fontfam <- setup_paper_theme(journal = "elsevier")

# ---- Datos ----------------------------------------------------------------

ejecutores <- c("gpt-oss:20b (Study 1)", "ornith:9b (replication)")

puntos <- read.csv("paper2/analysis/fig1-points.csv") |>
  as_tibble() |>
  mutate(
    ejecutor = factor(ejecutor, levels = ejecutores),
    etiqueta = toupper(tarea)
  )

# La Ec. (1) es una identidad con este estimador: si no cierra, el CSV no
# viene de la versión vigente del script.
stopifnot(
  nrow(puntos) == 6,
  all(abs(puntos$delta_t * puntos$r_mem - puntos$delta_r * puntos$t_base -
            puntos$net) < 0.01),
  all(abs(puntos$break_even -
            puntos$delta_t * puntos$r_mem / puntos$t_base) < 1e-6)
)

# ---- Frontera por ejecutor (banda entre sus 3 curvas por tarea) ----------

x_max <- 1.75
grilla <- puntos |>
  select(ejecutor, tarea, t_base, r_mem) |>
  crossing(delta_r = seq(0, x_max, by = 0.01)) |>
  mutate(frontera = delta_r * t_base / r_mem) |>
  group_by(ejecutor, delta_r) |>
  summarise(fmin = min(frontera), fmax = max(frontera), .groups = "drop")

# ---- Figura ---------------------------------------------------------------

col_ej <- c(pal_wong[6], pal_wong[7])  # azul (20B), bermellón (9B)
names(col_ej) <- ejecutores
forma_tarea <- c(original = 21, loop = 24, move = 22)

p <- ggplot() +
  geom_vline(xintercept = 0, linewidth = 0.3, colour = "grey60") +
  geom_ribbon(
    data = grilla,
    aes(x = delta_r, ymin = fmin, ymax = fmax, fill = ejecutor),
    alpha = 0.25, colour = NA, show.legend = FALSE
  ) +
  geom_errorbar(
    data = puntos,
    aes(x = delta_r, ymin = t_lo, ymax = t_hi, colour = ejecutor),
    width = 0, linewidth = 0.4
  ) +
  geom_errorbar(
    data = puntos,
    aes(y = delta_t, xmin = r_lo, xmax = r_hi, colour = ejecutor),
    orientation = "y", width = 0, linewidth = 0.4
  ) +
  geom_point(
    data = puntos,
    aes(x = delta_r, y = delta_t, fill = ejecutor, shape = tarea),
    size = 2.4, stroke = 0.45, colour = "black"
  ) +
  geom_text_repel(
    data = puntos,
    aes(x = delta_r, y = delta_t, label = etiqueta),
    family = fontfam, size = 2.3, seed = 42,
    box.padding = 0.45, point.padding = 0.3,
    min.segment.length = 0.2, segment.size = 0.25
  ) +
  annotate(
    "text", x = x_max, y = 40, hjust = 1, vjust = 0,
    label = "below the band: amortizes",
    family = fontfam, size = 2.4, colour = "grey30"
  ) +
  annotate(
    "text", x = -3.33, y = 760, hjust = 0, vjust = 1,
    label = "playbook adds rounds",
    family = fontfam, size = 2.4, colour = "grey30"
  ) +
  scale_colour_manual(values = col_ej, name = NULL) +
  scale_fill_manual(values = col_ej, guide = "none") +
  scale_shape_manual(values = forma_tarea, guide = "none") +
  guides(colour = guide_legend(override.aes = list(linewidth = 0.8))) +
  scale_x_continuous(breaks = seq(-3, 1.5, by = 0.5)) +
  scale_y_continuous(breaks = seq(0, 700, by = 100)) +
  # coord_cartesian recorta sin descartar datos (con limits la banda del
  # 9B se truncaba al salir del rango y).
  coord_cartesian(xlim = c(-3.4, x_max), ylim = c(0, 770),
                  expand = FALSE, clip = "on") +
  labs(
    x = "ΔR: rounds saved by the playbook",
    y = "ΔT: input tokens per round (playbook − none)"
  ) +
  theme(
    legend.position = "inside",
    legend.position.inside = c(0.02, 0.03),
    legend.justification = c(0, 0),
    legend.key.width = unit(5, "mm")
  )

# ---- Salidas (convención del Paper 1: pdf + png preview + caption + info) --

dir.create("paper2/figs", showWarnings = FALSE, recursive = TRUE)
ggsave("paper2/figs/fig1_frontera.pdf", p,
       width = 120, height = 82, units = "mm", device = cairo_pdf)
ggsave("paper2/figs/fig1_frontera_preview.png", p,
       width = 120, height = 82, units = "mm", dpi = 220)

be <- function(ej) {
  x <- puntos |> filter(ejecutor == ej) |> pull(break_even)
  sprintf("$%.2f$--$%.2f$", min(x), max(x))
}
writeLines(paste0(
  "\\caption{The amortization frontier ",
  "(Eq.~\\ref{eq:amortization}) on the $(\\Delta R, \\Delta T)$ plane, ",
  "for the three task pairs of Study~1 (gpt-oss:20b) and of the ",
  "replication (ornith:9b), $n=20$ per cell, computed from the ",
  "committed raw runs. Circle: \\textsc{original}; triangle: ",
  "\\textsc{loop}; square: \\textsc{move}. Horizontal bars: Welch ",
  "95\\% intervals on $\\Delta R$; vertical bars: percentile bootstrap ",
  "95\\% intervals on $\\Delta T$ (input tokens per round, total over ",
  "total in each arm). Each shaded band spans ",
  "one executor's per-task break-even lines $\\Delta T^{*} = \\Delta R ",
  "\\cdot T_{\\mathrm{base}} / R_{\\mathrm{mem}}$; a point below its ",
  "executor's band saves net tokens. Per-task break-even is ",
  be(ejecutores[1]), " rounds on gpt-oss:20b and ",
  be(ejecutores[2]), " on ornith:9b. Only \\textsc{original} on ",
  "gpt-oss:20b falls below its band, and its net saving is not ",
  "distinguishable from zero; on ornith:9b the playbook adds rounds on ",
  "every task.}"
), "paper2/figs/fig1_frontera_caption.tex")

writeLines(capture.output(sessionInfo()),
           "paper2/figs/fig1_frontera.sessioninfo.txt")

cat("fig1_frontera: escrita. Puntos:\n")
print(puntos |> select(ejecutor, tarea, delta_r, r_lo, r_hi,
                       delta_t, t_lo, t_hi, break_even, net))
