# Regenera figuras (desde los JSONs commiteados en docs/) y compila el PDF.
.PHONY: all figs pdf clean

all: figs pdf

figs:
	Rscript R/fig1_curva.R
	Rscript R/fig2_rescate.R
	Rscript R/fig3_search_tools.R

pdf:
	pdflatex -interaction=nonstopmode main.tex
	bibtex main
	pdflatex -interaction=nonstopmode main.tex
	pdflatex -interaction=nonstopmode main.tex

clean:
	rm -f main.aux main.bbl main.blg main.log main.out main.pdf
