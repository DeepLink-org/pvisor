"""Derive project brand assets from the owner-supplied raster artwork."""

from pathlib import Path

from PIL import Image


def generate() -> None:
    """Preserve the source artwork and create wordmark, favicon and share card."""
    directory = Path(__file__).resolve().parent
    with Image.open(directory / "logo.png") as original:
        source = original.convert("RGBA")
    if source.size != (2172, 724):
        raise ValueError("Source dimensions changed; review the artwork crop coordinates.")

    # Ignore near-transparent stray pixels when finding the full wordmark bounds.
    bounds = source.getchannel("A").point(lambda value: 255 if value >= 128 else 0).getbbox()
    if bounds is None:
        raise ValueError("The source artwork is empty.")
    left, top, right, bottom = bounds
    wordmark = source.crop((left - 4, top - 4, right + 4, bottom + 4))
    panel = Image.new("RGB", (wordmark.width + 96, wordmark.height + 64), "white")
    panel.paste(wordmark, (48, 32), wordmark)
    panel.save(directory / "pvisor-logo.png", optimize=True)

    # This rectangle isolates the original blue pi without any Visor lettering.
    mark = source.crop((340, 285, 737, 607))
    mark.thumbnail((432, 432), Image.Resampling.LANCZOS)
    icon = Image.new("RGBA", (512, 512))
    icon.alpha_composite(mark, ((512 - mark.width) // 2, (512 - mark.height) // 2))
    icon.save(directory / "pvisor-icon.png", optimize=True)

    wordmark.thumbnail((1040, 390), Image.Resampling.LANCZOS)
    social = Image.new("RGB", (1280, 630), "white")
    social.paste(wordmark, ((1280 - wordmark.width) // 2, (630 - wordmark.height) // 2), wordmark)
    social.save(directory / "pvisor-social.png", optimize=True)


if __name__ == "__main__":
    generate()
