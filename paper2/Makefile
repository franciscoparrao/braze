# Regenera figuras (desde los JSONs commiteados en docs/) y compila el PDF.
# Misma convención que paper/Makefile: los scripts R declaran rutas
# relativas a la RAÍZ del repo, por eso el cd .. antes de cada Rscript.
.PHONY: all figs pdf clean

all: figs pdf

figs:
	cd .. && Rscript paper2/R/fig1_frontera.R

pdf:
	pdflatex -interaction=nonstopmode main.tex
	pdflatex -interaction=nonstopmode main.tex

# bibtex entra cuando exista refs.bib (S3):
#	pdflatex main && bibtex main && pdflatex ×2

clean:
	rm -f main.aux main.bbl main.blg main.log main.out main.pdf
