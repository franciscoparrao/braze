# fig1_threshold.R — Figura central del Paper 3: el umbral manda.
#
# (a) Curva umbral δ (fracción de la suite) → tasa de aceptación falsa (FAR) sobre
#     los pares nulos de los incumbentes (campo curve_pooled_delta_vs_far).
# (b) FAR de las reglas de decisión publicadas sobre las 1.614 comparaciones
#     nulas del archivo (false_acceptance_rate).
# (c) Potencia con mejoras inyectadas: δ fijo de H0 vs re-derivado por
#     configuración, H0 al azar (200 sorteos); e = 0 es la FAR.
#
# Todo se lee de los JSON commiteados (nada hardcodeado), que a su vez salen del
# archivo congelado docs/paper3-archive-manifest.txt.
# Correr desde la raíz del repo:  Rscript paper3/R/fig1_threshold.R

suppressPackageStartupMessages({
  library(dplyr)
  library(tidyr)
  library(ggplot2)
  library(jsonlite)
  library(patchwork)
  library(forcats)
})
source("paper/R/theme_paper.R")
fontfam <- setup_paper_theme(journal = "springer")
dir.create("paper3/figs", showWarnings = FALSE)

# ---- Datos -----------------------------------------------------------------

curve <- fromJSON("docs/paper3-per-config-threshold.json")$curve_pooled_delta_vs_far |>
  as_tibble()

far <- fromJSON("docs/paper3-false-acceptance.json")$false_acceptance_rate
rules <- tibble(
  key = names(far), far = unlist(far)
) |>
  mutate(label = recode(key,
    meta_harness  = "Best score (Meta-Harness)",
    autodesign    = "Train up, dev not down (AutoDesign)",
    hcl_delta1    = "Margin 1 task + anchor (HCL)",
    hcl_delta3    = "Margin 3 tasks + anchor (HCL)",
    braze_mcnemar = "Exact paired test, α = 0.05"
  )) |>
  mutate(label = fct_reorder(label, far))

pw <- fromJSON("docs/paper3-v3-power.json")$random
power <- tibble(e = as.numeric(names(pw)),
                fixed = sapply(pw, `[[`, "fixed_h0"),
                per_config = sapply(pw, `[[`, "per_config")) |>
  pivot_longer(c(fixed, per_config), names_to = "rule", values_to = "share") |>
  mutate(rule = recode(rule, fixed = "Fixed (calibrated once on H0)",
                             per_config = "Re-derived per configuration"))

# Chequeos: la figura debe decir los mismos números que el texto.
stopifnot(abs(rules$far[rules$key == "meta_harness"] - 0.263) < 0.001,
          abs(rules$far[rules$key == "hcl_delta3"] - 0.011) < 0.001,
          abs(curve$far[curve$delta == 0.20] - 0.008) < 0.001)

pct <- scales::label_percent(accuracy = 1)

# ---- (a) curva -------------------------------------------------------------

pa <- ggplot(curve, aes(delta, far)) +
  geom_hline(yintercept = 0.05, linewidth = 0.25, linetype = "dashed", colour = "grey55") +
  annotate("text", x = 0.30, y = 0.05, label = "5%", hjust = 1, vjust = -0.5,
           size = 2.3, colour = "grey40", family = fontfam) +
  geom_line(linewidth = 0.5, colour = pal_wong[1]) +
  geom_point(size = 0.9, colour = pal_wong[1]) +
  scale_x_continuous(labels = pct, breaks = seq(0, 0.30, 0.05),
                     expand = expansion(mult = c(0.02, 0.02))) +
  scale_y_continuous(labels = pct, limits = c(0, NA),
                     expand = expansion(mult = c(0, 0.04))) +
  labs(x = "Threshold δ (share of the suite)", y = "False-acceptance rate")

# ---- (b) reglas publicadas -------------------------------------------------

pb <- ggplot(rules, aes(far, label)) +
  geom_segment(aes(x = 0.0003, xend = far, yend = label), linewidth = 0.3, colour = "grey70") +
  geom_point(size = 1.6, colour = pal_wong[6]) +
  geom_text(aes(label = scales::label_percent(accuracy = 0.1)(far)),
            hjust = -0.35, size = 2.3, family = fontfam) +
  scale_x_log10(labels = c("0.1%", "1%", "10%"),
                breaks = c(0.001, 0.01, 0.1), limits = c(0.0003, 1.5),
                expand = expansion(mult = c(0, 0))) +
  labs(x = "False-acceptance rate (log scale)", y = NULL)

# ---- (c) potencia ----------------------------------------------------------

pc <- ggplot(power, aes(e, share, colour = rule)) +
  geom_line(linewidth = 0.5) +
  geom_point(size = 0.9) +
  scale_colour_manual(values = c(pal_wong[2], pal_wong[3]), name = NULL) +
  scale_x_continuous(labels = pct, breaks = c(0, 0.05, 0.10, 0.15, 0.20),
                     expand = expansion(mult = c(0.02, 0.02))) +
  scale_y_continuous(labels = pct, limits = c(0, 1),
                     expand = expansion(mult = c(0, 0.10))) +
  labs(x = "Injected improvement e (share of the suite)", y = "Changes committed") +
  theme(legend.position = "inside", legend.position.inside = c(0.98, 0.04),
        legend.justification = c(1, 0), legend.key.width = unit(0.5, "cm"),
        legend.background = element_blank())

# ---- Composición -----------------------------------------------------------

fig <- (pa | pb) / pc +
  plot_layout(heights = c(1, 0.8)) +
  plot_annotation(tag_levels = "a", tag_suffix = ")") &
  theme(plot.tag = element_text(face = "bold"),
        plot.tag.location = "plot",
        plot.margin = margin(10, 6, 3, 3))

save_paper(fig, "paper3/figs/fig1_threshold.pdf", width_cm = 18, height_cm = 12)
sessionInfo()
