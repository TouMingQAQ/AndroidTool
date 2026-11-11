"""Rebuild the editable SVG, PNG and multi-resolution Windows icon (Pillow required)."""

from pathlib import Path

from PIL import Image, ImageDraw


ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / "assets"
SCALE = 4
image = Image.new("RGBA", (512 * SCALE, 512 * SCALE))
draw = ImageDraw.Draw(image)
elements = []


def box(bounds):
    return tuple(int(value * SCALE) for value in bounds)


def rounded_rect(bounds, radius, color):
    x, y, right, bottom = bounds
    draw.rounded_rectangle(box(bounds), radius=radius * SCALE, fill=color)
    elements.append(
        f'<rect x="{x}" y="{y}" width="{right-x}" height="{bottom-y}" '
        f'rx="{radius}" fill="{color}"/>'
    )


def line(points, color, width):
    scaled = [(int(x * SCALE), int(y * SCALE)) for x, y in points]
    draw.line(scaled, fill=color, width=width * SCALE, joint="curve")
    for x, y in points:
        radius = width / 2
        draw.ellipse(box((x-radius, y-radius, x+radius, y+radius)), fill=color)
    coords = " ".join(f"{x},{y}" for x, y in points)
    elements.append(
        f'<polyline points="{coords}" fill="none" stroke="{color}" '
        f'stroke-width="{width}" stroke-linecap="round" stroke-linejoin="round"/>'
    )


# Broad silhouettes and generous spacing remain readable at taskbar sizes.
rounded_rect((24, 24, 488, 488), 108, "#112D38")
rounded_rect((44, 44, 468, 468), 90, "#173B46")
line([(185, 174), (164, 137)], "#81EDB5", 18)
line([(327, 174), (348, 137)], "#81EDB5", 18)
rounded_rect((112, 168, 400, 372), 70, "#81EDB5")
rounded_rect((136, 214, 376, 334), 35, "#112D38")
line([(181, 247), (209, 273), (181, 299)], "#F0FFF6", 15)
line([(248, 300), (299, 300)], "#81EDB5", 15)
rounded_rect((228, 391, 284, 403), 6, "#81EDB5")

ASSETS.mkdir(exist_ok=True)
svg = (
    '<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" '
    'viewBox="0 0 512 512" role="img" aria-labelledby="title">\n'
    '<title id="title">AndroidTool - Android terminal</title>\n'
    + "\n".join(elements)
    + "\n</svg>\n"
)
(ASSETS / "androidtool.svg").write_text(svg, encoding="ascii")
image = image.resize((512, 512), Image.Resampling.LANCZOS)
image.save(ASSETS / "androidtool.png")
image.save(
    ASSETS / "androidtool.ico",
    sizes=[(size, size) for size in (16, 24, 32, 48, 64, 128, 256)],
)
print(f"Icon assets written to {ASSETS}")
