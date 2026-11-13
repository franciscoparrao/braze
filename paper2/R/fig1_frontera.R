# fig1_frontera.R — La frontera de amortización (figura central del Paper 2).
#
# Plano (ΔR = rondas ahorradas por el playbook, ΔT = diferencia de tokens
# de entrada por ronda, playbook − none): la curva de equilibrio de la
# ec. (1) del paper,
#   ΔT* = ΔR · T_base / R_mem
# separa la región donde la memoria amortiza (bajo la curva) de donde
# cuesta tokens netos (sobre ella).
#
# Revisión EMSE PROMPT-SE (B13, 2026-11-13): agrega la réplica en
# ornith:9b e intervalos en ambas dimensiones. Lee los MISMOS lotes que
# paper2/analysis/reanalysis.py (no el JSON combinado) para que figura y
# tablas salgan de una sola fuente; el stopifnot de abajo verifica contra
# docs/paper2-reanalysis-2026-11-06.md.
#   Estudio 1: docs/sweep-memory-distillation-r20-moveB-batch*-2026-07-17.json
#   Réplica:   docs/sweep-m1-ornith9b-batch*-2026-08-15.json
#
# Intervalos: IC 95 % de Welch sobre la diferencia de medias por corrida,
# tanto para ΔR (rondas, como el texto) como para ΔT (tokens/ronda por
# corrida). Brazos independientes, no pareados.
#
# La frontera de cada ejecutor se dibuja como BANDA: cada tarea tiene su
# propio T_base y R_mem; la banda cubre el rango de sus tres curvas. Solo
# para ΔR ≥ 0: con ΔR < 0 y ΔT > 0 la memoria cuesta tokens siempre.
#
# Correr desde la raíz del repo:  Rscript paper2/R/fig1_frontera.R

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

lee_lotes <- function(patron) {
  Sys.glob(patron) |>
    sort() |>
    map(\(p) fromJSON(p)$results |>
          as_tibble() |>
          select(task_id, rounds, input_tokens)) |>
    list_rbind()
}

ejecutores <- list(
  "gpt-oss:20b (Study 1)" =
    "docs/sweep-memory-distillation-r20-moveB-batch*-2026-07-17.json",
  "ornith:9b (replication)" =
    "docs/sweep-m1-ornith9b-batch*-2026-08-15.json"
)

tareas <- tribble(
  ~tarea,     ~base,
  "original", "rust_borrow_fix",
  "loop",     "rust_borrow_fix_loop",
  "move",     "rust_move_fix"
)

welch <- function(x, y) {
  # Diferencia y − x con IC 95 % de Welch (misma convención que reanalysis.py).
  t <- t.test(y, x, var.equal = FALSE)
  c(est = mean(y) - mean(x), lo = t$conf.int[1], hi = t$conf.int[2])
}

celda <- function(raw, base) {
  n0 <- raw |> filter(task_id == paste0(base, "_none"))
  n1 <- raw |> filter(task_id == paste0(base, "_human_playbook"))
  dR <- welch(n1$rounds, n0$rounds)  # rondas ahorradas = none − playbook
  dT <- welch(n0$input_tokens / n0$rounds, n1$input_tokens / n1$rounds)
  tibble(
    n_none = nrow(n0), n_pb = nrow(n1),
    delta_r = dR[["est"]], r_lo = dR[["lo"]], r_hi = dR[["hi"]],
    delta_t = dT[["est"]], t_lo = dT[["lo"]], t_hi = dT[["hi"]],
    t_base = mean(n0$input_tokens / n0$rounds),
    r_mem = mean(n1$rounds),
    net = mean(n1$input_tokens) - mean(n0$input_tokens)
  )
}

puntos <- imap(ejecutores, \(patron, ejecutor) {
  raw <- lee_lotes(patron)
  tareas |>
    mutate(ejecutor = ejecutor, datos = map(base, \(b) celda(raw, b)))
}) |>
  list_rbind() |>
  unnest(datos) |>
  mutate(
    ejecutor = factor(ejecutor, levels = names(ejecutores)),
    break_even = delta_t * r_mem / t_base,
    etiqueta = toupper(tarea)
  )

# Reproducción de docs/paper2-reanalysis-2026-11-06.md (redondeado allí).
# Si esto dispara, la figura no está leyendo lo que dicen las tablas.
ref <- tribble(
  ~ejecutor,                   ~tarea,     ~dR,   ~rlo,  ~rhi,  ~dT, ~be,
  "gpt-oss:20b (Study 1)",     "original", 0.95,  0.25,  1.65, 184, 0.75,
  "gpt-oss:20b (Study 1)",     "loop",     0.15, -0.40,  0.70, 243, 0.96,
  "gpt-oss:20b (Study 1)",     "move",     0.35,  0.02,  0.68, 268, 1.16,
  "ornith:9b (replication)",   "original", -0.50, -1.17, 0.17, 398, 1.01,
  "ornith:9b (replication)",   "loop",    -1.55, -2.16, -0.94, 582, 1.68,
  "ornith:9b (replication)",   "move",    -1.75, -3.26, -0.24, 527, 1.77
)
chk <- inner_join(puntos |> mutate(ejecutor = as.character(ejecutor)),
                  ref, by = c("ejecutor", "tarea"))
stopifnot(
  nrow(chk) == 6,
  all(puntos$n_none == 20), all(puntos$n_pb == 20),
  all(abs(chk$delta_r - chk$dR) < 0.006),
  all(abs(chk$r_lo - chk$rlo) < 0.006), all(abs(chk$r_hi - chk$rhi) < 0.006),
  all(abs(chk$delta_t - chk$dT) < 0.6),
  all(abs(chk$break_even - chk$be) < 0.006)
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
names(col_ej) <- names(ejecutores)
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
    "text", x = -3.33, y = 705, hjust = 0, vjust = 1,
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
  coord_cartesian(xlim = c(-3.4, x_max), ylim = c(0, 715),
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
  "\\textsc{loop}; square: \\textsc{move}. Bars are Welch 95\\% ",
  "intervals on the difference of per-run means, in rounds ($\\Delta R$) ",
  "and in input tokens per round ($\\Delta T$). Each shaded band spans ",
  "one executor's per-task break-even lines $\\Delta T^{*} = \\Delta R ",
  "\\cdot T_{\\mathrm{base}} / R_{\\mathrm{mem}}$; a point below its ",
  "executor's band saves net tokens. Per-task break-even is ",
  be(names(ejecutores)[1]), " rounds on gpt-oss:20b and ",
  be(names(ejecutores)[2]), " on ornith:9b. Only \\textsc{original} on ",
  "gpt-oss:20b falls below its band, and its net saving is not ",
  "distinguishable from zero; on ornith:9b the playbook adds rounds on ",
  "every task.}"
), "paper2/figs/fig1_frontera_caption.tex")

writeLines(capture.output(sessionInfo()),
           "paper2/figs/fig1_frontera.sessioninfo.txt")

cat("fig1_frontera: escrita. Puntos:\n")
print(puntos |> select(ejecutor, tarea, delta_r, r_lo, r_hi,
                       delta_t, t_lo, t_hi, break_even, net))
