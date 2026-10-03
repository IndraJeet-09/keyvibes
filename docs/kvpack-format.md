# KVPack Format Specification v1.0

**Status:** Stable  
**Date:** 2026-10-03  
**Version:** 1.0.0

## Overview

KVPack (`.kvpack`) is a binary sound pack format for KeyVibes. It provides:

- Portable, platform-independent representation
- Memory-mapped runtime access (zero-copy)
- Multiple sound variants per key
- Deterministic parsing and validation
- Corruption resistance
- Forward compatibility

## Design Principles

1. **Fixed endianness:** All integers are little-endian
2. **Explicit sizes:** No platform-dependent types (`usize`)
3. **No raw pointers:** Only offsets into the file
4. **Bounds validation:** All offsets and lengths checked
5. **Overflow safety:** Arithmetic verified with checked operations
6. **Zero-copy:** Samples accessed directly from mmap
7. **Immutable:** Pack is read-only after validation
8. **RT-safe:** Lookup requires no allocation, locking, or I/O

## File Structure

```
┌─────────────────────────────────┐  Offset 0
│ Magic (8 bytes)                 │  "KVPACK\0\0"
├─────────────────────────────────┤  8
│ Header fields (112 bytes)       │  Version, offsets, sizes
├─────────────────────────────────┤  120
│ Metadata (variable)             │  Name, author, license
├─────────────────────────────────┤  metadata_offset
│ Key Table (variable)            │  PhysicalKey → variants
├─────────────────────────────────┤  key_table_offset
│ Clip Table (variable)           │  Variant metadata
├─────────────────────────────────┤  clip_table_offset
│ [Alignment padding]             │
├─────────────────────────────────┤  sample_data_offset (aligned)
│ Sample Data                     │  PCM i16 samples
│   [guard][clip][guard]          │
│   [guard][clip][guard]          │
│   ...                           │
└─────────────────────────────────┘  EOF
```

## Magic Number

```
Offset: 0
Size:   8 bytes
Value:  0x4B 0x56 0x50 0x41 0x43 0x4B 0x00 0x00
        K    V    P    A    C    K    \0   \0
```

All KVPack files must start with this exact sequence.

## Header (120 bytes)

All integers are **little-endian**.

```
Offset  Size  Type   Field                Description
------  ----  -----  -------------------  ---------------------------------
0       8     u8[8]  magic                Magic: "KVPACK\0\0"
8       2     u16    format_version       Format version (1)
10      2     u16    flags                Reserved flags (0)
12      4     u32    header_size          Header size (120)
16      8     u64    file_size            Total file size in bytes
24      4     u32    key_count            Number of keys with sounds
28      4     u32    clip_count           Total number of clips (variants)
32      8     u64    metadata_offset      Offset to metadata section
40      4     u32    metadata_size        Size of metadata section
44      8     u64    key_table_offset     Offset to key table
52      4     u32    key_table_size       Size of key table (bytes)
56      8     u64    clip_table_offset    Offset to clip table
64      4     u32    clip_table_size      Size of clip table (bytes)
68      8     u64    sample_data_offset   Offset to sample data (aligned)
76      8     u64    sample_data_size     Size of sample data region
84      4     u32    sample_rate          Default sample rate (e.g. 48000)
88      2     u16    channels             Channels per clip (1 = mono)
90      2     u16    sample_format        Sample format (0 = i16 LE)
92      [28]  u8[]   reserved             Reserved (must be zero)
```

### Field Constraints

- `format_version`: Must be `1` for this specification
- `flags`: Must be `0` (reserved for future use)
- `header_size`: Must be `120`
- `file_size`: Must match actual file size
- `key_count`: Must be ≤ 104 (PhysicalKey::COUNT)
- `clip_count`: Must be ≤ 10,000 (MAX_CLIPS)
- `metadata_size`: Must be ≤ 65,536 bytes (MAX_METADATA_SIZE)
- `key_table_size`: Must equal `key_count * 8`
- `clip_table_size`: Must equal `clip_count * 24`
- `sample_data_offset`: Must be aligned to 16 bytes
- `sample_rate`: Must be 8,000 to 192,000
- `channels`: Must be `1` (mono)
- `sample_format`: Must be `0` (i16 little-endian)
- All `reserved` bytes must be `0`

### Offset Validation

All offsets must satisfy:
```
offset ≥ 120 (after header)
offset ≤ file_size
offset + size ≤ file_size (with overflow check)
```

## Metadata Section

```
Offset  Size       Type      Field
------  ---------  --------  --------
0       4          u32       name_len
4       name_len   u8[]      name (UTF-8)
...     4          u32       author_len
...     author_len u8[]      author (UTF-8)
...     4          u32       description_len
...     ...        u8[]      description (UTF-8)
...     4          u32       license_len
...     ...        u8[]      license (UTF-8)
...     4          u32       source_len
...     ...        u8[]      source (UTF-8)
```

Each string is:
- Length-prefixed (u32 little-endian)
- UTF-8 encoded
- Maximum 4,096 bytes per string
- Optional (length = 0 for omitted strings)

### String Validation

- Length must not cause offset to exceed `metadata_size`
- Must be valid UTF-8
- No null terminators required

## Key Table

Array of `key_count` entries, each 8 bytes:

```
Offset  Size  Type   Field          Description
------  ----  -----  -------------  ----------------------------
0       2     u16    physical_key   PhysicalKey discriminant (0-103)
2       2     u16    variant_count  Number of variants for this key
4       4     u32    first_clip     Index of first clip in clip table
```

### Key Entry Constraints

- `physical_key`: Must be < 104 (PhysicalKey::COUNT)
- `variant_count`: Must be > 0 and ≤ 16 (MAX_VARIANTS_PER_KEY)
- `first_clip`: Must be < `clip_count`
- `first_clip + variant_count`: Must be ≤ `clip_count`
- Entries must be sorted by `physical_key` (ascending)
- No duplicate `physical_key` values

## Clip Table

Array of `clip_count` entries, each 24 bytes:

```
Offset  Size  Type   Field              Description
------  ----  -----  -----------------  ----------------------------
0       8     u64    sample_offset      Offset into sample data region
8       4     u32    sample_frames      Logical clip length (frames)
12      4     u32    guard_before       Guard samples before (2)
16      4     u32    guard_after        Guard samples after (3)
20      4     u32    stored_frames      Total stored frames (with guards)
```

### Clip Entry Constraints

- `sample_offset`: Relative to `sample_data_offset`
- `sample_offset`: Must be < `sample_data_size`
- `stored_frames`: Must equal `guard_before + sample_frames + guard_after`
- `stored_frames * sizeof(i16)`: Must fit in `sample_data_size - sample_offset`
- `sample_frames`: Must be > 0 and ≤ 10,000,000 (MAX_CLIP_FRAMES)
- `guard_before`: Must be `2`
- `guard_after`: Must be `3`

## Sample Data

Contiguous region of PCM samples:

```
Format:        Signed 16-bit little-endian (i16)
Channels:      1 (mono)
Sample rate:   Specified in header
Layout:        [guard_before][sample_frames][guard_after]
Alignment:     sample_data_offset % 16 == 0
```

### Guard Samples

Each clip is stored with guard samples for safe interpolation:

```
[guard][guard][sample_0][sample_1]...[sample_n][guard][guard][guard]
  ^     ^                                         ^      ^      ^
  2 before                                        3 after
```

Guard samples typically contain:
- Before: Copy of first frame or zero
- After: Copy of last frame or zero

The mixer's cubic interpolator can safely read ±1 sample beyond the logical clip boundaries.

### Sample Validation

- All samples must be finite (no NaN/Inf)
- Range: -32,768 to +32,767 (i16)
- Total size: `clip_count * stored_frames * sizeof(i16)`

## PhysicalKey Stable IDs

PhysicalKey discriminants are **frozen** and must never change:

```rust
Escape = 0
F1 = 1
...
Pause = 103
```

This mapping is part of the format specification. Pack files rely on these values remaining stable across KeyVibes versions.

## Maximum Limits

To prevent resource exhaustion:

```rust
MAX_KEYS = 104                    // PhysicalKey::COUNT
MAX_CLIPS = 10_000                // Total variants across all keys
MAX_VARIANTS_PER_KEY = 16         // Variants per individual key
MAX_CLIP_FRAMES = 10_000_000      // ~3 minutes at 48kHz
MAX_METADATA_SIZE = 65_536        // 64 KB
MAX_STRING_SIZE = 4_096           // 4 KB per string
MAX_PACK_SIZE = 2_147_483_648     // 2 GB
```

## Validation Algorithm

1. **Magic check:** First 8 bytes must be `KVPACK\0\0`
2. **Version check:** `format_version == 1`
3. **File size:** Actual size must match `file_size`
4. **Limits:** All counts must be ≤ their MAX_*
5. **Offsets:** All offsets must be valid and non-overlapping
6. **Alignment:** `sample_data_offset % 16 == 0`
7. **Key table:** Sorted, no duplicates, valid physical keys
8. **Clip table:** All offsets and lengths valid
9. **Metadata:** Valid UTF-8, lengths within bounds
10. **Overflow:** All arithmetic checked for overflow

## Runtime Lookup

### O(1) Key Lookup

```rust
PhysicalKey (u16)
    ↓
Binary search key_table (cached for common keys)
    ↓
KeyEntry { first_clip, variant_count }
    ↓
Select random variant in [first_clip, first_clip + variant_count)
    ↓
ClipEntry { sample_offset, sample_frames, ... }
    ↓
&sample_data[sample_offset..sample_offset + stored_frames]
```

### Variant Selection

For keys with multiple variants:

1. Use lightweight PRNG (xorshift or similar)
2. Avoid immediately repeating same variant
3. Deterministic with seed for testing
4. No allocation, locking, or I/O

## Memory Mapping

```
File on disk
    ↓
mmap (read-only, MADV_RANDOM or MADV_WILLNEED)
    ↓
Validate entire structure
    ↓
Create immutable Pack object
    ↓
Runtime sample access (zero-copy slices)
```

### Lifetime Guarantee

- Pack owns the mmap
- Sample slices are valid while Pack is alive
- Voices must not outlive their source Pack
- Pack switching uses generation or refcounting

## Corruption Resistance

The format defends against:

- Truncated files (size mismatch)
- Invalid offsets (bounds checks)
- Integer overflow (checked arithmetic)
- Out-of-bounds clips (offset + length validation)
- Invalid UTF-8 (explicit validation)
- Absurd counts (maximum limits)
- Recursive structures (N/A - flat format)
- Format confusion (magic number)

## Forward Compatibility

Future versions may:

- Add new fields to header `reserved` space
- Define new `flags` bits
- Support additional sample formats
- Support stereo/multichannel
- Add compression (with explicit format codes)
- Add checksums or signatures

Loaders must:

- Reject unknown `format_version` major
- Ignore unknown `flags` bits (if minor-compatible)
- Skip unknown metadata strings
- Preserve forward compatibility where documented

## Example Pack Structure

```
Header (120 bytes)
  magic: "KVPACK\0\0"
  format_version: 1
  key_count: 3
  clip_count: 7
  sample_rate: 48000
  channels: 1

Metadata (variable)
  name: "Example Mechanical"
  author: "KeyVibes"
  license: "CC0"

Key Table (24 bytes = 3 × 8)
  Entry 0: PhysicalKey::A (67), 3 variants, clips [0,1,2]
  Entry 1: PhysicalKey::S (68), 2 variants, clips [3,4]
  Entry 2: PhysicalKey::Space (98), 2 variants, clips [5,6]

Clip Table (168 bytes = 7 × 24)
  Clip 0: offset 0, 4800 frames
  Clip 1: offset 9610, 4750 frames
  ...

Sample Data (aligned to 16)
  [guard][guard][A variant 0 PCM][guard][guard][guard]
  [guard][guard][A variant 1 PCM][guard][guard][guard]
  ...
```

## Security Considerations

- No executable code or scripts
- No path traversal (metadata is data only)
- No dynamic loading or eval
- Integer overflow protection
- Bounds checking on all array access
- UTF-8 validation prevents malformed strings

## Builder Reproducibility

Builders should:

- Sort keys by discriminant
- Sort clips by key order
- Use deterministic padding
- Omit timestamps (unless explicitly requested)
- Avoid machine-specific paths
- Allow explicit metadata

This enables `SHA256(pack_a) == SHA256(pack_b)` for identical inputs.

## References

- KeyVibes PhysicalKey enumeration (kv-core)
- PlayCommand format (kv-core)
- Mixer voice interpolation (kv-mixer)
- Lock-free SPSC queue (kv-ring)

---

**Version History:**

- v1.0.0 (2026-10-03): Initial stable specification
