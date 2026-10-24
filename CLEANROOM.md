# Clean-room policy

**English** | [简体中文](CLEANROOM.zh-CN.md)

Quire is an independent implementation. The goal is functional similarity with
common word processors, not similarity of code, data or appearance.

## What contributors may use

1. Public specifications: ISO/IEC 29500 (OOXML), ODF, RTF, PDF, Unicode (UAX #9,
   #14, #29), OpenType, CSS/SVG where applicable.
2. Public behaviour: what a user can observe by using an application.
3. This repository's own fixtures and test corpus.
4. Publicly documented APIs of the Rust crates we depend on.

## What is forbidden

- Copying decompiled code, pseudocode, symbol names or string constants from a
  proprietary binary into this repository.
- Copying icons, skins, splash screens, fonts, templates or other assets.
- Copying UI screenshots pixel-for-pixel, or reproducing proprietary layout
  parameters as if they were specification.
- Adding account, cloud, advertising, telemetry or DRM code, even behind a
  feature flag.

## Provenance for contributions

Every non-trivial pull request should state the source of its behaviour:

- `spec: <section>` — implemented from a public specification.
- `observation: <description>` — implemented from user-observable behaviour.
- `design: <document>` — an original design decision.

If a contribution cannot name a permitted source, it is not merged.

## Reverse-engineering evidence

Any structural notes kept while investigating an existing application (module
inventory, imports, exports, type names, strings) document responsibilities only.
They are not a source of implementation, and they must not include decompiled
code or copied constants.

## Naming and marks

The project does not use the names, logos or trademarks of any existing office
suite. Compatibility statements are limited to format support ("reads DOCX"),
never to brand endorsement.
