# PolicyVisor brand assets

`logo.png` is the original, owner-supplied π + Visor artwork. Preserve its
letterforms, colors and relative alignment. It is a raster original; do not
replace it with a traced or font-generated approximation.

Run `python3 docs/overrides/assets/logos/generate.py` from the repository root
with Pillow installed to regenerate:

- `pvisor-logo.png`: the complete wordmark, with transparent margins trimmed
  and a white backing for legibility in both light and dark README/site themes.
- `pvisor-icon.png`: the original blue π, fitted onto a transparent 512 × 512
  square for the favicon. No lettering is redrawn.
- `pvisor-social.png`: a 1280 × 630 white sharing card using the full wordmark.

The crop coordinates belong to this specific source image. If the source is
replaced, review the crops and inspect the wordmark and favicon before publishing.
The alpha threshold only locates the artwork, ignoring nearly transparent stray
pixels in the original margins; the cropped pixels retain their original alpha.
