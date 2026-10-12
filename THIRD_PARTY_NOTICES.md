# Third-party notices

KeyVibes itself is original work. This file lists the third-party material
that ships with, or is used to build, the sound packs under
`assets/soundpacks/upstream-src/`, and where the attribution and license for
each item live. Nothing else in this repository contains third-party code or
audio.

## Sound pack audio (21 packs)

The 21 packs converted by
[`scripts/import_upstream_soundpacks.py`](scripts/import_upstream_soundpacks.py)
are built from audio and key-mapping data packaged by
[`KunalBagaria/rustyvibes-v2`](https://github.com/KunalBagaria/rustyvibes-v2)
at commit `b1f3584319a6c7a31f5f7c29f21baba4c08148d9` (2026-10-02). The audio
in that checkout originates from the two projects below; both are MIT
licensed and both notices are reproduced in full.

The conversion pipeline, the pack manifests, and the `.kvpack` files KeyVibes
produces are KeyVibes work; only the audio samples and the per-key scan-code
mappings come from the upstream projects.

### Mechvibes (11 packs)

- **What**: press and release samples for the `mechvibes/*` packs
  (Cherry MX ABS/PBT, Everglide, ToPRE variants).
- **Upstream**: [hainguyents13/mechvibes](https://github.com/hainguyents13/mechvibes)
  at commit `326252a13e7bef4f1c35d08ef0189b5af6f8ba02`.
- **License**: MIT, Copyright (c) 2021 Hai Nguyen.
- **License text**: see `assets/soundpacks/upstream-src/upstream-licenses/mechvibes-LICENSE`.

```
MIT License

Copyright (c) 2021 Hai Nguyen

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

### kbsim (10 packs)

- **What**: press and release samples for the `kbsim/*` packs (Alpaca,
  Buckling Spring, Gateron, Holy Panda, Kailh, NovelKeys, SKCM Alps, ToPRE,
  Tealios).
- **Upstream**: [tplai/kbsim](https://github.com/tplai/kbsim) at commit
  `ba103f3b0afa9dab80447aa2e7e2ed80b6bd80e4`.
- **License**: MIT, Copyright (c) Thomas Lai.
- **License text**: see `assets/soundpacks/upstream-src/upstream-licenses/kbsim-LICENSE.md`.

```
Copyright (c) Thomas Lai

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

### rustyvibes-v2 (packaging and mapping reference)

- **What**: the 21-pack catalog (`assets/soundpacks/catalog.json`), the
  per-pack key-to-scan-code mappings (`config.json` for the mechvibes packs,
  `press/`+`release/` file naming for the kbsim packs), and the conversion
  approach KeyVibes' importer ports (sprite slicing, onset-aware trim,
  normalization). No audio file is copied from this repository unconverted;
  the importer re-decodes, trims, normalizes, and re-encodes every clip as
  16-bit WAV.
- **Upstream**: [KunalBagaria/rustyvibes-v2](https://github.com/KunalBagaria/rustyvibes-v2)
  at commit `b1f3584319a6c7a31f5f7c29f21baba4c08148d9` (2026-10-02).
- **License**: MIT, Copyright (c) 2021–2026 Kunal Bagaria.
- **License text**: see `assets/soundpacks/upstream-src/upstream-licenses/rustyvibes-v2-LICENSE`.

```
MIT License

Copyright (c) 2021–2026 Kunal Bagaria

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Synthesized packs (no third-party audio)

The `Default`, `Holy Panda`, and `Linear` demo packs under
`assets/soundpacks/default-src/` are fully synthesized by
[`scripts/generate_sounds.py`](scripts/generate_sounds.py) (key-impact
physical model, pure Python + numpy). Their manifests say
`source = "keyvibes-synth-switch-v1"`. No third-party audio is involved.
