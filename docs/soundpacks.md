# Sound packs

A pack is a `.kvpack` file: metadata, a key table, an optional release
table, a clip table, and the sample data, all in one file that is validated
once and then memory-mapped. Sample rate, channel count, and sample format
are fixed at build time, so the runtime never converts anything on the
real-time path.

This page covers the packs KeyVibes ships with and how they are made. The
binary format itself is specified in [kvpack-format.md](kvpack-format.md),
and the builder API is documented in [kvpack-builder.md](kvpack-builder.md).

## Upstream mechanical-switch packs (21)

Converted from the pinned
[`KunalBagaria/rustyvibes-v2`](https://github.com/KunalBagaria/rustyvibes-v2)
checkout (commit `b1f3584...`) by
[`scripts/import_upstream_soundpacks.py`](../scripts/import_upstream_soundpacks.py).
The audio originates from [Mechvibes](https://github.com/hainguyents13/mechvibes)
(Hai Nguyen) and [kbsim](https://github.com/tplai/kbsim) (Thomas Lai); all
three projects are MIT licensed. Full attribution and license texts are in
[THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md) and in
`assets/soundpacks/upstream-src/upstream-licenses/`.

| Category  | Packs                                                                                                                       |
| --------- | --------------------------------------------------------------------------------------------------------------------------- |
| `linear`  | Alpaca, Cherry MX Black/Red ABS+PBT, Everglide Crystal Purple, Everglide Oreo, Gateron Black/Red Ink, NovelKeys Cream, Turquoise Tealios |
| `tactile` | Cherry MX Brown ABS+PBT, Holy Panda (kbsim), SKCM Blue Alps, ToPRE, ToPRE Purple Hybrid PBT                                    |
| `clicky`  | Cherry MX Blue ABS+PBT, Buckling Spring, Kailh Box Navy                                                                       |

(The `.kvpack` metadata's `category` field is the machine-readable form;
`pack list` prints it in brackets.)

**Release sounds.** The ten kbsim packs and one Mechvibes pack
(`cherry-mx-black-abs`) carry a release clip for most or all keys, stored in
the version 2 release table. Releasing a key plays that clip when the
`release_sounds` setting is on (off by default; see `keyvibes config show`).
A key with no release clip - or a version 1 pack - simply stays silent on
release; nothing is ever substituted.

**Coverage is honest.** The Mechvibes packs do not define `PrintScreen` or
`Pause` (and `ToPRE Purple Hybrid PBT` also lacks `F8`), so those keys are
missing from those packs. A press of a missing key makes no sound and is
counted (`Presses with no pack sound`); it is **never** served another key's
clip, and no A-key fallback exists. The exact per-pack coverage is recorded
in `assets/soundpacks/upstream-src/coverage.json`, written by the importer
from what it actually converted - not from what upstream claims.

## Rebuilding the upstream packs

The converted sources (manifests + slice WAVs) are committed; the `.kvpack`
files are generated. From the repository root:

```bash
# Re-convert from a pinned rustyvibes-v2 checkout (regenerates coverage.json):
python3 scripts/import_upstream_soundpacks.py --source /path/to/rustyvibes-v2

# Build every converted pack and install it for the current user:
for d in assets/soundpacks/upstream-src/*/; do
  [ -f "$d/pack.toml" ] || continue
  cargo run --quiet --bin keyvibes -- pack build "$d/pack.toml" \
    -o "$HOME/.local/share/keyvibes/packs/$(basename "$d").kvpack"
done
```

Build output is deterministic: the same manifest always produces the same
bytes, which is what the acceptance test's rebuild check asserts.

## Verifying the packs

```bash
keyvibes pack list            # names, categories, key/clip/release counts
keyvibes pack list -v         # plus coverage N/104 per pack
keyvibes pack inspect <path>  # layout, coverage, release table, key table
keyvibes pack validate <path> # full structural validation
```

`keyvibes soundpack-acceptance-test` is the gate for the whole family: nine
checks over the catalog, installation, provenance and license, honest
coverage, no cross-key substitution, clip audio, release-sound semantics, a
byte-identical rebuild, and a simulated press/release run through the live
engine with PipeWire. Steps that need a resource the host does not have
report `NOT RUN`, never a pass.

```bash
keyvibes soundpack-acceptance-test
```

## Installing packs

Packs are searched in this order:

1. `$KEYVIBES_PACK_DIR`
2. `$XDG_DATA_HOME/keyvibes/packs`
3. `~/.local/share/keyvibes/packs`
4. `/usr/local/share/keyvibes/packs`
5. `/usr/share/keyvibes/packs`
6. `assets/soundpacks` (relative to the current directory)

A prefix outside that list needs `KEYVIBES_PACK_DIR` set. Discovery skips
test fixtures (anything under a `tests/fixtures` directory, or named
`golden*`), so golden test packs can never become the default; `--pack
<path>` still opens one deliberately. If two installed packs share a name,
the first one found wins - which is why the kbsim pack ships as
**Holy Panda (kbsim)**: names must stay unique across everything a user may
have installed.

```bash
keyvibes --pack "Holy Panda (kbsim)" run
keyvibes --pack "Cherry MX Red PBT" run
```
