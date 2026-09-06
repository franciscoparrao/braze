# fig1_frontera.R — La frontera de amortización (figura central del Paper 2).
#
# Plano (ΔR = rondas ahorradas por el playbook, ΔT = costo fijo de
# inyección por ronda): la curva de equilibrio de la ec. (1) del paper,
#   ΔT* = ΔR · T_base / R_mem
# separa la región donde la memoria amortiza (bajo la curva) de donde
# cuesta tokens netos (sobre ella). Los tres pares del piloto M1 se
# computan DESDE EL JSON CRUDO commiteado (no se hardcodean):
#   docs/sweep-memory-distillation-r20-moveB-2026-07-17.json  (140 corridas)
# Análisis de referencia: docs/sweep-memory-distillation-3taskB-
# synthesis-2026-07-17.md (tabla que esta figura debe reproducir).
#
# La curva se dibuja como BANDA: cada tarea tiene su propia frontera
# (T_base y R_mem propios); la banda cubre el rango de las tres — más
# honesto que una curva única con parámetros promediados.
#
# Correr desde la raíz del repo:  Rscript paper2/R/fig1_frontera.R

suppressPackageStartupMessages({
  library(dplyr)
  library(tidyr)
  library(ggplot2)
  library(jsonlite)
  library(ggrepel)
})
source("paper/R/theme_paper.R")
fontfam <- setup_paper_theme(journal = "elsevier")

# ---- Datos ----------------------------------------------------------------

raw <- fromJSON("docs/sweep-memory-distillation-r20-moveB-2026-07-17.json")$results |>
  as_tibble()

# Los 3 pares B del piloto (el holdout H no entra en esta figura).
pares <- tribble(
  ~tarea,      ~none_id,                  ~pb_id,
  "original",  "rust_borrow_fix_none",      "rust_borrow_fix_human_playbook",
  "loop",      "rust_borrow_fix_loop_none", "rust_borrow_fix_loop_human_playbook",
  "move",      "rust_move_fix_none",        "rust_move_fix_human_playbook"
)

celda <- function(task_id) {
  raw |>
    filter(task_id == !!task_id) |>
    summarise(
      n = n(),
      # Media de razones por-corrida: la agregación de la tabla de la
      # síntesis (verificado contra el JSON: 1406.0→1589.8 ⇒ ΔT≈184 en
      # ORIGINAL) — figura y tabla del paper deben decir el mismo número.
      # ANTES de `rounds = mean(rounds)`: summarise evalúa secuencial y
      # la columna nueva pisaría a la cruda (bug encontrado: daba
      # razón-de-medias, 181, en vez de 184).
      tok_por_ronda = mean(input_tokens / rounds),
      rounds = mean(rounds),
      in_tok = mean(input_tokens)
    )
}

puntos <- pares |>
  rowwise() |>
  mutate(
    none = list(celda(none_id)),
    pb   = list(celda(pb_id))
  ) |>
  unnest_wider(none, names_sep = "_") |>
  unnest_wider(pb, names_sep = "_") |>
  mutate(
    delta_r = none_rounds - pb_rounds,                # rondas ahorradas
    delta_t = pb_tok_por_ronda - none_tok_por_ronda,  # costo fijo por ronda
    net_delta = pb_in_tok - none_in_tok,              # balance neto (tokens)
    amortiza = net_delta < 0,
    etiqueta = sprintf(
      "%s\n(net %+d tok)", toupper(tarea), round(net_delta)
    )
  ) |>
  ungroup()

# Chequeo de reproducción contra la síntesis commiteada (tolerancias
# holgadas: la síntesis redondea). Si esto dispara, la figura no está
# leyendo lo que la tabla del paper dice.
stopifnot(
  all(puntos$none_n == 20), all(puntos$pb_n == 20),
  abs(puntos$delta_r[puntos$tarea == "original"] - 0.95) < 0.05,
  abs(puntos$net_delta[puntos$tarea == "original"] - (-304)) < 60,
  puntos$net_delta[puntos$tarea == "loop"] > 900,
  puntos$net_delta[puntos$tarea == "move"] > 900
)

# ---- Frontera (banda entre las 3 curvas por-tarea) ------------------------

grilla <- tidyr::crossing(
  delta_r = seq(0, 1.15, by = 0.01),
  puntos |> select(tarea, none_tok_por_ronda, pb_rounds)
) |>
  mutate(frontera = delta_r * none_tok_por_ronda / pb_rounds) |>
  group_by(delta_r) |>
  summarise(fmin = min(frontera), fmax = max(frontera), fmed = mean(frontera))

# ---- Figura ---------------------------------------------------------------

p <- ggplot() +
  geom_ribbon(
    data = grilla,
    aes(x = delta_r, ymin = fmin, ymax = fmax),
    fill = "grey80", alpha = 0.5
  ) +
  geom_line(
    data = grilla, aes(x = delta_r, y = fmed),
    linewidth = 0.4, linetype = "22", colour = "grey30"
  ) +
  annotate(
    "text", x = 1.02, y = 88, hjust = 1, vjust = 0,
    label = "amortizes\n(saves net tokens)",
    family = fontfam, size = 2.7, colour = "grey25", lineheight = 0.95
  ) +
  annotate(
    "text", x = 0.02, y = 297, hjust = 0, vjust = 1,
    label = "costs net tokens",
    family = fontfam, size = 2.7, colour = "grey25"
  ) +
  geom_point(
    data = puntos,
    aes(x = delta_r, y = delta_t, shape = amortiza, fill = amortiza),
    size = 2.6, stroke = 0.5, colour = "black"
  ) +
  geom_text_repel(
    data = puntos,
    aes(x = delta_r, y = delta_t, label = etiqueta),
    family = fontfam, size = 2.6, lineheight = 0.95,
    seed = 42, box.padding = 0.5, min.segment.length = 0.15,
    segment.size = 0.25
  ) +
  scale_shape_manual(values = c(`TRUE` = 21, `FALSE` = 24), guide = "none") +
  scale_fill_manual(
    values = c(`TRUE` = pal_wong[4], `FALSE` = pal_wong[7]),
    guide = "none"
  ) +
  scale_x_continuous(
    expand = expansion(mult = c(0.01, 0.03)),
    breaks = seq(0, 1, by = 0.25)
  ) +
  scale_y_continuous(expand = expansion(mult = c(0.01, 0.05))) +
  labs(
    x = expression(paste(Delta, "R: rounds saved by the playbook")),
    y = expression(paste(Delta, "T: injection overhead (tokens/round)"))
  )

# ---- Salidas (convención del Paper 1: pdf + png preview + caption + info) --

dir.create("paper2/figs", showWarnings = FALSE, recursive = TRUE)
ggsave("paper2/figs/fig1_frontera.pdf", p,
       width = 120, height = 78, units = "mm", device = cairo_pdf)
ggsave("paper2/figs/fig1_frontera_preview.png", p,
       width = 120, height = 78, units = "mm", dpi = 220)

writeLines(sprintf(
  paste0(
    "\\caption{The amortization frontier "
    , "(Eq.~\\ref{eq:amortization}) on the $(\\Delta R, \\Delta T)$ "
    , "plane, with the three task pairs of Study 1 ($n=20$ per cell, "
    , "gpt-oss:20b) "
    , "computed from the committed raw runs. The shaded band spans the "
    , "per-task break-even curves $\\Delta T^{*} = \\Delta R \\cdot "
    , "T_{\\mathrm{base}} / R_{\\mathrm{mem}}$, each computed from that "
    , "task's measured $(T_{\\mathrm{base}}, R_{\\mathrm{mem}})$ under "
    , "the none arm. Only the task the model "
    , "already has memorized (\\textsc{original}, circle, net $-304$ "
    , "tokens) falls on the amortizing side; the two fresh tasks "
    , "(triangles) sit $3$--$6\\times$ above their break-even, costing "
    , "$+1076$ and $+1132$ net tokens per task.}"
  )
), "paper2/figs/fig1_frontera_caption.tex")

writeLines(capture.output(sessionInfo()),
           "paper2/figs/fig1_frontera.sessioninfo.txt")

cat("fig1_frontera: escrita. Puntos:\n")
print(puntos |> select(tarea, delta_r, delta_t, net_delta, amortiza))
