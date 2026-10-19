#!/usr/bin/env bash
# Mostra os pacotes recomendados para o Claudio Code no LMDE / Debian.
# Este script NÃO executa sudo: copie e rode o comando você mesmo.
cat <<'TXT'
Essenciais (compilação, PDF, documentos, mídia, isolamento):
  sudo apt install build-essential cmake git pkg-config poppler-utils pandoc ffmpeg bubblewrap

Opcionais:
  sudo apt install libreoffice-writer libreoffice-calc libreoffice-impress   # conversões e PDF ricos
  sudo apt install texlive-latex-recommended texlive-lang-portuguese         # LaTeX
  sudo apt install tesseract-ocr tesseract-ocr-por                           # OCR
  sudo apt install espeak-ng alsa-utils                                      # voz
  sudo apt install chromium                                                  # navegador headless
  sudo apt install libvulkan-dev glslc mesa-vulkan-drivers vulkan-tools      # llama.cpp na GPU AMD
  sudo apt install libfuse2                                                  # executar AppImage
TXT
