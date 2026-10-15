#!/usr/bin/env python3
"""Report a deterministic stream/resource size breakdown for classic-xref PDFs.

Use a producer's diagnostic classic-xref mode when comparing payload and
object categories. For an existing PDF 1.5/object-stream file, normalize it
first with ``mutool clean input.pdf normalized.pdf``.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import zlib
from collections import defaultdict
from pathlib import Path


SCHEMA = 1


def fail(message: str) -> None:
    raise ValueError(message)


def classic_objects(data: bytes) -> dict[int, bytes]:
    marker = data.rfind(b"startxref")
    if marker < 0:
        fail("missing startxref")
    try:
        xref = int(data[marker + len(b"startxref"):].split()[0])
    except (IndexError, ValueError):
        fail("invalid startxref")
    if not data[xref:].startswith(b"xref"):
        fail("not a classic-xref PDF; normalize it with mutool clean first")

    position = xref + len(b"xref")

    def line() -> bytes:
        nonlocal position
        while position < len(data) and data[position] in b"\r\n":
            position += 1
        end = data.find(b"\n", position)
        if end < 0:
            end = len(data)
        value = data[position:end].rstrip(b"\r")
        position = end + 1
        return value

    offsets: dict[int, int] = {}
    while True:
        header = line()
        if header == b"trailer":
            break
        fields = header.split()
        if len(fields) != 2:
            fail("invalid xref subsection")
        try:
            first, count = map(int, fields)
        except ValueError:
            fail("invalid xref subsection numbers")
        for number in range(first, first + count):
            row = line().split()
            if len(row) < 3:
                fail("truncated xref row")
            if row[2] == b"n":
                offsets[number] = int(row[0])

    ordered = sorted((offset, number) for number, offset in offsets.items())
    objects: dict[int, bytes] = {}
    for index, (offset, number) in enumerate(ordered):
        end = ordered[index + 1][0] if index + 1 < len(ordered) else xref
        expected = f"{number} ".encode()
        if not data[offset:].startswith(expected):
            fail(f"xref does not point to object {number}")
        objects[number] = data[offset:end]
    return objects


def object_prefix(body: bytes) -> bytes:
    match = re.search(rb"\bstream\r?\n", body)
    return body if match is None else body[:match.start()]


def indirect_references(pattern: bytes, prefixes: list[bytes]) -> set[int]:
    return {
        int(match)
        for prefix in prefixes
        for match in re.findall(pattern, prefix, re.DOTALL)
    }


def analyze(path: Path) -> dict[str, object]:
    data = path.read_bytes()
    objects = classic_objects(data)
    prefixes = [object_prefix(body) for body in objects.values()]

    font_programs = indirect_references(
        rb"/FontFile(?:2|3)?\s+(\d+)\s+\d+\s+R", prefixes
    )
    to_unicode = indirect_references(
        rb"/ToUnicode\s+(\d+)\s+\d+\s+R", prefixes
    )
    classic_font_dictionaries = [
        prefix for prefix in prefixes
        if re.search(rb"/Type\s*/Font\s*/Subtype\s*/(?:Type1|Type3)\b", prefix)
    ]
    descriptors = [
        prefix for prefix in prefixes
        if re.search(rb"/Type\s*/FontDescriptor\b", prefix)
    ]
    face_names: set[bytes] = set()
    encodings: set[bytes] = set()
    for prefix in classic_font_dictionaries:
        name = re.search(rb"/(?:BaseFont|Name)\s*/([^\s/<>{}\[\]()]+)", prefix)
        if name:
            face_names.add(re.sub(rb"^[A-Z]{6}\+", b"", name.group(1)))
        encoding = re.search(
            rb"/Encoding\s*(/[^\s/<>{}\[\]()]+|<<.*?>>)", prefix, re.DOTALL
        )
        if encoding:
            encodings.add(re.sub(rb"\s+", b" ", encoding.group(1)).strip())
    page_contents: set[int] = set()
    char_procedures: set[int] = set()
    for prefix in prefixes:
        if re.search(rb"/Type\s*/Page(?!s)\b", prefix):
            for direct, array in re.findall(
                rb"/Contents(?:\s+(\d+)\s+\d+\s+R|\s*\[(.*?)\])",
                prefix,
                re.DOTALL,
            ):
                if direct:
                    page_contents.add(int(direct))
                else:
                    page_contents.update(
                        int(value)
                        for value in re.findall(rb"(\d+)\s+\d+\s+R", array)
                    )
        for dictionary in re.findall(rb"/CharProcs\s*<<(.*?)>>", prefix, re.DOTALL):
            char_procedures.update(
                int(value)
                for value in re.findall(rb"(\d+)\s+\d+\s+R", dictionary)
            )

    categories: dict[str, dict[str, int]] = defaultdict(
        lambda: {"objects": 0, "stored_bytes": 0, "decoded_bytes": 0,
                 "flate_streams": 0}
    )
    stream_count = 0
    stored_stream_bytes = 0
    decoded_stream_bytes = 0
    flate_streams = 0
    undecoded_filtered_streams = 0
    images: list[dict[str, object]] = []
    forms: list[dict[str, object]] = []

    for number, body in objects.items():
        stream = re.search(rb"\bstream\r?\n", body)
        if stream is None:
            continue
        prefix = body[:stream.start()]
        length = re.search(rb"/Length\s+(\d+)\b", prefix)
        if length is None:
            fail(f"object {number} has a non-direct stream length")
        stored = int(length.group(1))
        payload = body[stream.end():stream.end() + stored]
        if len(payload) != stored:
            fail(f"object {number} has a truncated stream")

        is_flate = re.search(rb"/Filter\s*/FlateDecode\b", prefix) is not None
        decoded = stored
        if is_flate:
            try:
                decoded = len(zlib.decompress(payload))
            except zlib.error:
                undecoded_filtered_streams += 1

        if number in font_programs or b"/Length1 " in prefix:
            category = "font_program"
        elif number in to_unicode:
            category = "to_unicode"
        elif number in char_procedures:
            category = "type3_charproc"
        elif re.search(rb"/Subtype\s*/Image\b", prefix):
            category = "image"
        elif re.search(rb"/Subtype\s*/Form\b", prefix):
            category = "form"
        elif number in page_contents:
            category = "page_content"
        elif re.search(rb"/Type\s*/ObjStm\b", prefix):
            category = "object_stream"
        elif re.search(rb"/Type\s*/XRef\b", prefix):
            category = "xref_stream"
        else:
            category = "other"

        values = categories[category]
        values["objects"] += 1
        values["stored_bytes"] += stored
        values["decoded_bytes"] += decoded
        values["flate_streams"] += int(is_flate)
        stream_count += 1
        stored_stream_bytes += stored
        decoded_stream_bytes += decoded
        flate_streams += int(is_flate)

        if category == "image":
            def integer(name: bytes) -> int | None:
                match = re.search(rb"/" + name + rb"\s+(\d+)\b", prefix)
                return int(match.group(1)) if match else None

            color = re.search(
                rb"/ColorSpace\s+(\[[^\]]+\]|/[A-Za-z0-9]+|\d+\s+\d+\s+R)",
                prefix,
                re.DOTALL,
            )
            filters = [
                name
                for name in (
                    "FlateDecode", "DCTDecode", "JPXDecode", "CCITTFaxDecode"
                )
                if f"/{name}".encode() in prefix
            ]
            mask = re.search(rb"/(SMask|Mask)\s+(\d+\s+\d+\s+R|\[[^\]]*\])", prefix)
            images.append({
                "object": number,
                "width": integer(b"Width"),
                "height": integer(b"Height"),
                "bits_per_component": integer(b"BitsPerComponent"),
                "color_space": (
                    b" ".join(color.group(1).split()).decode("ascii", "replace")
                    if color else None
                ),
                "filters": filters,
                "stored_bytes": stored,
                "decoded_bytes": decoded,
                "mask": (
                    f"/{mask.group(1).decode()} "
                    + b" ".join(mask.group(2).split()).decode("ascii", "replace")
                    if mask else None
                ),
            })
        elif category == "form":
            bbox = re.search(rb"/BBox\s*\[([^\]]+)\]", prefix)
            forms.append({
                "object": number,
                "stored_bytes": stored,
                "decoded_bytes": decoded,
                "bbox": (
                    b" ".join(bbox.group(1).split()).decode("ascii", "replace")
                    if bbox else None
                ),
            })

    return {
        "schema": SCHEMA,
        "path": str(path),
        "sha256": hashlib.sha256(data).hexdigest(),
        "pdf_bytes": len(data),
        "objects": len(objects),
        "non_payload_bytes": len(data) - stored_stream_bytes,
        "streams": {
            "objects": stream_count,
            "stored_bytes": stored_stream_bytes,
            "decoded_bytes": decoded_stream_bytes,
            "flate_streams": flate_streams,
            "undecoded_filtered_streams": undecoded_filtered_streams,
        },
        "classic_fonts": {
            "physical_faces": len(face_names),
            "embedded_programs": len(font_programs),
            "descriptors": len(descriptors),
            "encodings": len(encodings),
            "pdf_fonts": len(classic_font_dictionaries),
        },
        "categories": dict(sorted(categories.items())),
        "images": sorted(images, key=lambda image: int(image["object"])),
        "forms": sorted(forms, key=lambda form: int(form["object"])),
    }


def human(result: dict[str, object]) -> None:
    print(f"{result['path']}: {result['pdf_bytes']:,} bytes, "
          f"{result['objects']} objects")
    fonts = result["classic_fonts"]
    print(
        "  classic fonts    "
        f"{fonts['physical_faces']} faces, {fonts['embedded_programs']} programs, "
        f"{fonts['descriptors']} descriptors, {fonts['encodings']} encodings, "
        f"{fonts['pdf_fonts']} PDF fonts"
    )
    for name, values in result["categories"].items():
        print(
            f"  {name:16} {values['objects']:5} streams  "
            f"{values['stored_bytes']:12,} stored  "
            f"{values['decoded_bytes']:12,} decoded  "
            f"{values['flate_streams']:5} Flate"
        )
    for image in result["images"]:
        print(
            f"    image {image['object']:5} {image['width']}x{image['height']} "
            f"{image['color_space']} {image['bits_per_component']}bpc "
            f"{'+'.join(image['filters']) or 'raw'} {image['stored_bytes']:,} bytes"
            + (f" {image['mask']}" if image["mask"] else "")
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pdf", nargs="+", type=Path)
    parser.add_argument("--json", action="store_true", help="emit schema-1 JSON")
    arguments = parser.parse_args()
    try:
        results = [analyze(path) for path in arguments.pdf]
    except (OSError, ValueError) as error:
        print(f"pdf size: {error}", file=sys.stderr)
        return 2
    if arguments.json:
        print(json.dumps({"schema": SCHEMA, "results": results}, indent=2,
                         sort_keys=True))
    else:
        for result in results:
            human(result)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
