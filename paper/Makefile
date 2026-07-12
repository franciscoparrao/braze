# Regenera figuras (desde los JSONs commiteados en docs/) y compila el PDF.
.PHONY: all figs pdf clean

all: figs pdf

# Los scripts declaran sus rutas relativas a la RAÍZ del repo ("Correr
# desde la raíz del repo", ver el encabezado de cada uno): leen
# docs/sweep-*.json y escriben paper/figs/ — por eso el cd .. antes de
# cada Rscript, este Makefile vive en paper/.
figs:
	cd .. && Rscript paper/R/fig1_curva.R
	cd .. && Rscript paper/R/fig2_rescate.R
	cd .. && Rscript paper/R/fig3_search_tools.R

pdf:
	pdflatex -interaction=nonstopmode main.tex
	bibtex main
	pdflatex -interaction=nonstopmode main.tex
	pdflatex -interaction=nonstopmode main.tex

clean:
	rm -f main.aux main.bbl main.blg main.log main.out main.pdf
