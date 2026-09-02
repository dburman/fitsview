# fitsview — Execution Plan

`fitsview` is a fast, cross-platform (Linux, macOS, Windows) desktop viewer for FITS
files used in astrophotography. It is written in Rust.

This README is an **execution plan**. It is self-contained: a developer can pick it
up and build the application phase by phase without needing additional context.
Work through the phases **in order**. Each phase ends with a checklist of
acceptance criteria. Do not begin the next phase until the current phase's
criteria all pass.

---

## 0. Product Summary

**Status:** Phase 0 complete. Phase 1 is next.

| Phase | State |
|-------|-------|
| 0 — Bootstrap | Done |
| 1 — FITS reader | Not started |
| 2 — Minimal viewer | Not started |
| 3 — Folder browsing | Not started |
| 4 — Delete, rename, flag | Not started |
| 5 — Stretch | Not started |
| 6 — Dark calibration | Not started |
| 7 — Bias calibration | Not started |
| 8 — Packaging | Not started |

Keep this table current. Phase 0 is project bootstrap; phases 1 through 8
deliver the features below.

| # | Requirement | Phase |
|---|-------------|-------|
| a | Open FITS files quickly. Performance is the top priority. | 1, 2 |
| b | Open a folder of FITS files; ignore non-FITS files. | 3 |
| c1 | Delete files with a toggle button **and** a keyboard shortcut. | 4 |
| c2 | Rename files quickly. | 4 |
| d | Flag images to "keep". Flagged files need extra confirmation before deletion. | 4 |
| e | Button to apply a standard astro stretch to all viewed images; can be turned off. | 5 |
| f | Dark-frame calibration applied to all files in the folder. | 6 |
| g | Bias-frame calibration applied to all files in the folder. | 7 |

---

## 1. Rules for the Builder (read before writing any code)

These rules apply to **every** phase. They override anything else in this document.

### 1.1 No `unsafe`

- Put `#![forbid(unsafe_code)]` at the top of `lib.rs` and `main.rs` in **both** crates. The compiler will then refuse any `unsafe` block.
- Do **not** use `memmap2`, raw pointer casts, `transmute`, `from_raw_parts`, or `bytemuck::cast_slice` in our code. Use the safe patterns in section 5.3 instead. They are fast enough.
- Dependencies may contain their own audited `unsafe` (e.g. `wgpu`, `rayon`). That is acceptable. Prefer a crate that declares `#![forbid(unsafe_code)]` when two crates do the same job.
- The `forbid` attribute is the real guarantee; a CI step also greps the source for the word `unsafe`. `cargo geiger` gives a dependency-tree view and is nice to have, but it is unmaintained and may fail to build. Do not block a phase on it.
- If you believe a task is **impossible or unreasonably slow** without `unsafe`, **stop and ask the maintainer** (see 1.2). Include: what you tried, a benchmark number for the safe version, and the expected gain. Do not write the `unsafe` code before asking.

### 1.2 Always ask before …

Stop and ask the maintainer (do not guess, do not proceed) before any of the following:

| Situation | What to say when asking |
|-----------|-------------------------|
| Using `unsafe` anywhere | Why, safe alternative tried, measured numbers. |
| Adding a crate that is not in the library table (section 3) | Crate name, version, what it replaces, its `unsafe` status, download count. |
| Changing the GUI framework, FITS reader strategy, or repo layout | What and why. |
| Hard-deleting, overwriting, or moving any of the user's image files outside the documented behaviour (trash-only deletes, `_cal.fits` exports) | Exactly which files. |
| Changing the sidecar `.fitsview.json` format after Phase 4 ships | Old and new schema, migration. |
| Skipping or weakening a test, or marking a test `#[ignore]` | Which test and why. |
| Deviating from a phase's acceptance criteria | Which criterion and why. |

Format the question as a short list: **what you want to do, why, the alternatives, your recommendation.** Then wait.

### 1.3 Always write tests

- **Every** new public function in `fits-core` gets at least one unit test in the same file (`#[cfg(test)] mod tests`).
- **Every** action in `actions.rs` (delete, rename, flag, sidecar read/write) gets an integration test using a `tempfile::TempDir`.
- Every phase's acceptance checklist is turned into tests where a test is possible. If a criterion can only be verified by hand (e.g. "dialog appears"), write a manual test note in `docs/manual-tests.md` and tick it off.
- Bug fixes come with a regression test that fails before the fix and passes after.
- Keep the GUI thin. All state transitions live in plain structs (`app::Model`, `folder::Folder`, `loader::Cache`) that can be tested **without** creating a window. Rendering code only reads that state and forwards input events to it.
- Tests must be deterministic and must not depend on files outside the repo. Generate FITS data with the synthetic generator in `fits-core/src/testutil.rs` (see 5.4).
- `cargo test --workspace` must be green on Linux, macOS, and Windows before a phase is considered done.
- Use `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check` as part of "tests pass".

### 1.4 Be thorough

- Read the whole phase before starting it. Read the FITS primer (section 4) before Phase 1.
- Handle every error path with a typed error; never `unwrap()`/`expect()` on data that came from a file, a dialog, or the user.
- Log timing for load, convert, stretch, texture upload with `log::debug!` so performance regressions are visible.
- When a phase is done, write a short summary: what was built, what tests exist, measured timings, and open questions.
- One commit (or one pull request) per phase, with the phase number in the subject, e.g. `Phase 2: minimal viewer with zoom and pan`. Commit messages describe the change and nothing else: no tool, generator, or authorship footers.

---

## 2. Architecture

### 2.1 Layer diagram

```mermaid
flowchart TB
    subgraph UI["UI layer (crate fitsview, egui) — no business logic"]
        TB[toolbar.rs]
        FL[filelist.rs]
        VW[viewer.rs]
        DG[dialogs.rs<br/>rename / confirm / help / calibration]
    end

    subgraph MODEL["Application model (crate fitsview) — pure state, fully unit-tested"]
        APP[app.rs<br/>Model: selection, view state,<br/>toggles, toasts]
        FO[folder.rs<br/>Folder, FileEntry, scan, natural sort]
        AC[actions.rs<br/>delete / rename / flag / sidecar]
        LD[loader.rs<br/>worker thread, LRU cache, prefetch]
        TX[texture.rs<br/>f32 → ColorImage → TextureHandle]
    end

    subgraph CORE["fits-core (library) — no GUI deps, #![forbid(unsafe_code)]"]
        HD[header.rs]
        RD[reader.rs<br/>read_fits / write_fits]
        IM[image.rs<br/>FitsImage, pixel conversion]
        ST[stretch.rs<br/>MTF auto-stretch, LUT]
        CB[calib.rs<br/>master median, dark/bias subtract]
        TU[testutil.rs<br/>synthetic FITS generator]
    end

    subgraph OS["Operating system"]
        FS[(File system)]
        TR[(Trash / Recycle bin)]
        GPU[(GPU via wgpu)]
        DLG[(Native dialogs)]
    end

    TB & FL & VW & DG --> APP
    APP --> FO & AC & LD & TX
    LD --> RD
    LD --> CB
    TX --> ST
    RD --> HD & IM
    CB --> IM
    RD --> FS
    AC --> FS & TR
    TX --> GPU
    TB --> DLG
```

### 2.2 Display pipeline (what happens per image)

```mermaid
flowchart LR
    F[FITS file on disk] -->|std::fs::read| B[Vec&lt;u8&gt;]
    B -->|header.rs| H[FitsHeader + data offset]
    B -->|image.rs<br/>rayon, from_be_bytes| I[FitsImage f32<br/>+ min/max]
    I -->|calib.rs<br/>optional, Phase 6/7| C[Calibrated FitsImage]
    C -->|stretch.rs<br/>optional, 64K LUT| L[u8 mapping]
    L -->|texture.rs<br/>+ downsample if &gt; 4096px| CI[egui ColorImage]
    CI -->|ctx.load_texture| T[TextureHandle]
    T --> V[viewer.rs draws quad<br/>with zoom / pan]
```

Caches sit at three points, all bounded LRU keyed by path:
`raw FitsImage` (after I), `calibrated FitsImage` (after C, only when calibration is on), and `TextureHandle` (after T, keyed by path + stretch/calibration settings hash).

### 2.3 Threads and channels

```mermaid
sequenceDiagram
    participant UI as UI thread (egui update loop)
    participant LW as Loader worker thread
    participant EW as Export worker thread (Phase 6+)

    UI->>LW: Request{path, generation} via mpsc::Sender
    Note over LW: fs::read → parse → convert (rayon pool)
    LW-->>UI: Response{path, generation, Result<Arc<FitsImage>>}
    Note over UI: poll() with try_recv each frame; request_repaint on arrival
    UI->>LW: Prefetch i+1, i-1
    UI->>EW: ExportJob{files, masters, out_dir, cancel: Arc<AtomicBool>}
    EW-->>UI: Progress{done, total} / Finished / Error
```

Rules:
- The UI thread never performs file I/O or pixel math larger than a single texture upload.
- Exactly one loader worker. Requests carry a `generation` counter; responses with a stale generation are dropped.
- `rayon`'s global pool is used inside the workers for pixel loops. The UI thread does not call into `rayon`.
- All shared images are `Arc<FitsImage>` (immutable after load). No `Mutex` around pixel data.

### 2.4 Text diagram (for tools that cannot render Mermaid)

```
┌───────────────────────────────────────────────────────────────────┐
│  fitsview (binary)                                                │
│                                                                   │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐             │
│  │ toolbar  │ │ filelist │ │  viewer  │ │ dialogs  │  UI (egui)   │
│  └────┬─────┘ └────┬─────┘ └────┬─────┘ └────┬─────┘             │
│       └────────────┴─────┬──────┴────────────┘                   │
│                          ▼                                        │
│  ┌──────────────────────────────────────────────────────────┐    │
│  │ app.rs  Model (selection, toggles, view state, toasts)   │    │
│  └───┬───────────────┬───────────────┬───────────────┬──────┘    │
│      ▼               ▼               ▼               ▼           │
│  folder.rs       actions.rs      loader.rs       texture.rs      │
│  scan, sort,     delete→trash    worker thread   f32→RGBA        │
│  sidecar         rename, flag    LRU cache       downsample      │
│                                  prefetch        LUT stretch     │
└──────┬───────────────┬───────────────┬───────────────┬──────────┘
       │               │               │               │
       ▼               ▼               ▼               ▼
┌───────────────────────────────────────────────────────────────────┐
│  fits-core (library, no GUI, forbid(unsafe_code))                 │
│  header.rs  reader.rs  image.rs  stretch.rs  calib.rs  testutil   │
└──────┬────────────────────────────────────────────────────────────┘
       ▼
  File system / Trash / GPU (wgpu) / native dialogs (rfd)
```

---

## 3. Suggested Libraries

Do not add crates outside this table without asking (rule 1.2).

**On versions:** the numbers below were current when this plan was written. Before
Phase 1, run `cargo add <crate>` for each one, let Cargo pick the current release,
and record the versions you actually got in `Cargo.toml`. `egui`/`eframe` move
fast and their `0.x` releases contain breaking changes, so take the newest `0.x`
pair and keep them on the same version as each other. Commit `Cargo.lock`.

### 3.1 Runtime dependencies

| Crate | Version | Used in | Purpose | `unsafe` notes | Alternative considered |
|-------|---------|---------|---------|----------------|------------------------|
| `eframe` | latest 0.x | fitsview | Window, event loop, persistence (`Storage`), wgpu/glow backend. | Contains unsafe internally (GPU/OS bindings). Unavoidable for any GUI. | `iced` (less mature image viewer story), `tauri` (needs webview + JS), `slint` (licence). |
| `egui` | same as `eframe` | fitsview | Immediate-mode widgets, `ColorImage`, `TextureHandle`. | Same as above. | — |
| `rayon` | 1.10 | both | Data-parallel pixel loops, parallel reduce for min/max. | Audited unsafe internally; API is safe. | `std::thread::scope` (more code, same result). |
| `rfd` | latest 0.x | fitsview | Native open-file / open-folder / message dialogs on all three OSes. | Wraps OS APIs. | `native-dialog` (fewer features). |
| `trash` | 5 | fitsview | Move files to OS trash / recycle bin; restore where supported. | Wraps OS APIs. | `std::fs::remove_file` — rejected: permanent deletion. |
| `serde` + `serde_json` | 1 | fitsview | Read/write `.fitsview.json` sidecar (flags, calibration paths). | `forbid(unsafe_code)` in serde_json. | Hand-rolled JSON — rejected. |
| `natord` | 1.0 | fitsview | Natural sort (`light_2` before `light_10`). Tiny, no deps. | Pure safe Rust. | Hand-written comparator (fine too). |
| `thiserror` | 2 | fits-core | Typed error enums. | Proc-macro, safe. | — |
| `anyhow` | 1 | fitsview | Error propagation in the app. | Safe. | — |
| `log` + `env_logger` | 0.4 / 0.11 | both | Logging with timing info. | Safe. | `tracing` (heavier than needed). |

**Not used, on purpose:**
- `memmap2` — requires `unsafe`. `std::fs::read` into a `Vec<u8>` is used instead; see 5.3 for why this is fast enough.
- `fitsio` — links C `cfitsio`; painful on Windows; unsafe FFI.
- `fitrs` / `fits-rs` — unmaintained, slower, incomplete BITPIX support.
- `bytemuck` — its casts are safe at the API level but are exactly the pattern we want to avoid reasoning about; `from_be_bytes` on chunks is clearer and equally fast.
- `tokio` / async — a single worker thread plus `std::sync::mpsc` is simpler and sufficient.
- `lru` crate — a 30-line `VecDeque`-based LRU in `loader.rs` avoids a dependency and is easier to test.
- `egui-notify` for toasts — a hand-rolled toast list (text + expiry `Instant`) is ~40 lines. May be added later with permission.

### 3.2 Development dependencies

| Crate | Version | Used in | Purpose |
|-------|---------|---------|---------|
| `tempfile` | 3 | both | Temporary directories for file-operation tests. |
| `criterion` | 0.5 | fits-core | Benchmarks for `read_fits`, `convert_pixels`, `compute_stretch`, `build_master_median`. |
| `proptest` | 1 | fits-core | Property tests: header parser never panics on arbitrary 2880-byte blocks; pixel round-trip write→read is identity. |
| `approx` | 0.5 | fits-core | `assert_relative_eq!` for float comparisons in stretch/calibration tests. |

### 3.3 Tooling (not dependencies)

| Tool | Purpose |
|------|---------|
| `cargo clippy`, `cargo fmt` | Required clean before each phase closes. |
| `cargo geiger` | Reports `unsafe` usage in our crates and dependencies. Run per phase. |
| `cargo llvm-cov` | Optional coverage report; aim for > 85 % in `fits-core`. |
| `cargo flamegraph` | Profile before optimising. |
| `cargo-bundle` (macOS), `winres` (Windows) | Phase 8 packaging. Optional. |

### 3.4 Summary of fixed decisions

| Concern | Choice | Why |
|---------|--------|-----|
| Language | Rust, stable toolchain, edition 2021 | Required. |
| `unsafe` | Forbidden in our crates (`#![forbid(unsafe_code)]`) | Rule 1.1. |
| GUI framework | [`egui`](https://crates.io/crates/egui) via [`eframe`](https://crates.io/crates/eframe) | Pure Rust, immediate-mode, one codebase for Linux/macOS/Windows, GPU-backed via `wgpu`, very fast to iterate on. |
| FITS parsing | **Custom minimal reader** (see Phase 1) | Existing crates (`fitsio` needs a C library `cfitsio`; `fitrs` is unmaintained). A hand-written reader for the image subset of FITS is ~300 lines, has no C dependency, no `unsafe`, and is the fastest option. |
| Image data type | `f32` per pixel, single or 3-channel | Covers BITPIX 8/16/32/-32/-64 after conversion; fast SIMD-friendly math. |
| Parallelism | [`rayon`](https://crates.io/crates/rayon) | Parallel pixel conversion, stretch, and calibration. |
| Concurrency | `std::thread` + `std::sync::mpsc` (no async runtime) | Keep it simple. A worker thread loads files; the UI thread never blocks. |
| File I/O | `std::fs::read` into `Vec<u8>` | Safe, one allocation, fast enough (see 5.3). `memmap2` rejected because it needs `unsafe`. |
| Byte order | `from_be_bytes` on fixed-size chunks | FITS data is always big-endian. No extra crate. |
| Native dialogs | [`rfd`](https://crates.io/crates/rfd) | Cross-platform open-file / open-folder dialogs. |
| Trash (soft delete) | [`trash`](https://crates.io/crates/trash) | Deleting moves files to OS trash/recycle bin. Safer than `fs::remove_file`. |
| Logging | `log` + `env_logger` | Standard. |
| Errors | `anyhow` (app) + `thiserror` (library) | Standard. |
| Tests | `cargo test`, with small synthetic FITS files generated in code | No binary fixtures in repo. |

**Do not add** other GUI frameworks, `tokio`, C-dependency crates, or anything using `unsafe` in our code (ask first, rule 1.2).

### 3.5 Repository Layout

```
fitsview/
├── Cargo.toml              # workspace
├── README.md               # this file
├── crates/
│   ├── fits-core/          # library: FITS parsing, stretch, calibration. NO GUI CODE.
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs      # #![forbid(unsafe_code)]
│   │       ├── header.rs   # FITS header parsing
│   │       ├── image.rs    # FitsImage struct + pixel conversion
│   │       ├── reader.rs   # read_fits / write_fits
│   │       ├── stretch.rs  # Phase 5
│   │       ├── calib.rs    # Phase 6 & 7
│   │       └── testutil.rs # synthetic FITS generator (feature "test-util")
│   └── fitsview/           # binary: the GUI app
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs     # #![forbid(unsafe_code)]
│           ├── app.rs      # eframe::App implementation + testable Model struct
│           ├── folder.rs   # folder scanning + file list model
│           ├── loader.rs   # background loading thread + cache
│           ├── texture.rs  # f32 image -> egui texture
│           ├── ui/
│           │   ├── mod.rs
│           │   ├── toolbar.rs
│           │   ├── filelist.rs
│           │   ├── viewer.rs
│           │   └── dialogs.rs
│           └── actions.rs  # delete / rename / flag logic
├── docs/
│   └── manual-tests.md     # per-phase manual test checklist for UI-only criteria
├── scripts/
│   └── check-unsafe.sh     # unsafe guard, run by CI and locally
├── rust-toolchain.toml     # pins the stable channel plus rustfmt and clippy
├── Cargo.lock              # committed: this workspace produces a binary
└── .github/workflows/ci.yml
```

Rule: `fits-core` must compile and test **without** any GUI dependency. This
keeps parsing/stretch/calibration testable and fast to iterate.

---

## 4. FITS Format — Minimum You Must Know

A FITS file is a sequence of **HDUs** (Header Data Units). We only need the
**primary HDU** for single images. Some cameras write the image in the first
extension instead; handle that as a fallback.

**Header:**
- Made of 80-byte ASCII "cards". Cards come in 2880-byte blocks.
- Card format: `KEYWORD = value / comment`. Keyword is bytes 0–7, `= ` at bytes 8–9.
- Header ends at a card whose keyword is `END`. Then pad to the next 2880-byte boundary.
- Required keywords for an image: `SIMPLE`, `BITPIX`, `NAXIS`, `NAXIS1`, `NAXIS2`, optionally `NAXIS3`.
- Scaling keywords: `BZERO` (default 0.0), `BSCALE` (default 1.0). Physical value = `BZERO + BSCALE * raw`.
  - Common case: `BITPIX=16`, `BZERO=32768` → this means the data is really unsigned 16-bit.
- Optional: `BAYERPAT` (e.g. `RGGB`) means it is a color camera raw. For this plan we display it as mono. Debayering is out of scope.

**Data:**
- Starts right after the padded header.
- Big-endian. Always, for every BITPIX.
- `BITPIX` values: `8` (u8), `16` (i16), `32` (i32), `64` (i64), `-32` (f32), `-64` (f64).
- Pixel count = `NAXIS1 * NAXIS2 * (NAXIS3 or 1)`. Row-major, `NAXIS1` is width.
- Data section is also padded to a 2880-byte boundary.
- If `NAXIS3 == 3` treat as RGB planes (plane-major: all R, then all G, then all B).

**Row order (important, easy to get wrong):** FITS stores the **bottom** row of the
image first. Screen coordinates put row 0 at the top. So the image must be flipped
vertically for display, or it will appear upside down compared with every other
viewer. Do the flip once, in `texture.rs`, when building the `ColorImage`; leave
`FitsImage.data` in native FITS order so that calibration frames line up with
lights without any flipping. Write a test that asserts the flip happens exactly
once.

**NaN and infinities:** `BITPIX = -32` and `-64` files can legitimately contain
`NaN` (undefined pixels) and infinities. Every statistic (`min`, `max`, median,
MAD, histogram) must skip non-finite values, and the display must map them to a
fixed colour (use black). A single `NaN` reaching a naive `min`/`max` will make
the whole image render blank, which is a confusing bug to chase later.

**Integer blanks:** for integer BITPIX, the optional `BLANK` keyword names the
value that means "no data". Treat it as `NaN` after conversion. Rare; handle it
because it is two lines of code.

**Detecting a FITS file:** first 6 bytes are `SIMPLE`, **and** extension is one
of `.fits`, `.fit`, `.fts` (case-insensitive). Check the extension first (cheap),
then verify the magic bytes.

---

## 5. Core Data Structures, Safe I/O, and Test Plan

### 5.1 Types (define in `fits-core` in Phase 1)

```rust
/// A parsed FITS header. Keep it simple: ordered list of cards.
pub struct FitsHeader {
    pub cards: Vec<(String, String)>, // (keyword, raw value string)
}
impl FitsHeader {
    pub fn get(&self, key: &str) -> Option<&str>;
    pub fn get_i64(&self, key: &str) -> Option<i64>;
    pub fn get_f64(&self, key: &str) -> Option<f64>;
}

/// The decoded image, always f32.
pub struct FitsImage {
    pub width: usize,
    pub height: usize,
    pub channels: usize,      // 1 or 3
    pub data: Vec<f32>,       // len = width * height * channels; plane-major if channels==3
    pub header: FitsHeader,
    pub min: f32,             // computed at load time
    pub max: f32,
}
```

### 5.2 Errors and entry point

```rust
#[derive(thiserror::Error, Debug)]
pub enum FitsError {
    #[error("not a FITS file")] NotFits,
    #[error("unsupported BITPIX {0}")] UnsupportedBitpix(i64),
    #[error("truncated data")] Truncated,
    #[error("io: {0}")] Io(#[from] std::io::Error),
    #[error("bad header: {0}")] BadHeader(String),
}

pub fn read_fits(path: &std::path::Path) -> Result<FitsImage, FitsError>;

/// Everything needed to decode the data block, validated once so the hot loop
/// can assume it is correct.
pub struct Geometry {
    pub width: usize,
    pub height: usize,
    pub channels: usize,     // 1 or 3
    pub bitpix: i64,         // one of 8, 16, 32, 64, -32, -64
    pub bzero: f64,
    pub bscale: f64,
    pub blank_as_f32: Option<f32>, // BLANK keyword, integer BITPIX only
}
impl Geometry {
    /// Rejects unsupported BITPIX, NAXIS outside 2..=3, zero or negative axis
    /// lengths, and NAXIS3 that is neither 1 nor 3.
    pub fn from_header(h: &FitsHeader) -> Result<Geometry, FitsError>;
    pub fn pixel_count(&self) -> usize;      // width * height * channels
    pub fn bytes_per_pixel(&self) -> usize;  // bitpix.unsigned_abs() / 8
}
```

---

### 5.3 Safe, fast file reading (replaces memory-mapping)

```rust
// reader.rs — no unsafe anywhere
pub fn read_fits(path: &Path) -> Result<FitsImage, FitsError> {
    let t0 = std::time::Instant::now();
    let bytes = std::fs::read(path)?;              // one allocation; ~10-40 ms for 50 MB from page cache
    if !bytes.starts_with(b"SIMPLE") {
        return Err(FitsError::NotFits);
    }
    let (header, data_start) = header::parse(&bytes)?;
    let geom = Geometry::from_header(&header)?;    // validates BITPIX and NAXIS; width, height, channels, bzero, bscale

    // Checked arithmetic: NAXIS values come from the file and can be absurd.
    let n_bytes = geom
        .pixel_count()
        .checked_mul(geom.bytes_per_pixel())
        .ok_or(FitsError::Truncated)?;
    let end = data_start.checked_add(n_bytes).ok_or(FitsError::Truncated)?;
    let data = bytes.get(data_start..end).ok_or(FitsError::Truncated)?;

    let mut out = vec![0f32; geom.pixel_count()];
    image::convert_pixels(&geom, data, &mut out);  // rayon inside
    let (min, max) = image::finite_min_max(&out);  // rayon reduce, skips NaN/inf
    log::debug!("read_fits {:?} in {:?}", path.file_name(), t0.elapsed());
    Ok(FitsImage { width: geom.width, height: geom.height, channels: geom.channels,
                   data: out, header, min, max })
}
```

```rust
// image.rs — safe big-endian conversion, parallel.
// The BITPIX match is hoisted OUT of the pixel loop: branching per pixel costs
// roughly 30 % here. Each arm is a tight, vectorisable loop.
use rayon::prelude::*;

const CHUNK: usize = 65_536; // pixels per rayon task

pub fn convert_pixels(geom: &Geometry, raw: &[u8], out: &mut [f32]) {
    let bpp = geom.bytes_per_pixel();
    let (bzero, bscale) = (geom.bzero, geom.bscale);
    // Unsigned-16 is the overwhelmingly common case from astro cameras.
    let fast_u16 = geom.bitpix == 16 && bzero == 32768.0 && bscale == 1.0;
    let scaled = !fast_u16 && (bzero != 0.0 || bscale != 1.0);

    out.par_chunks_mut(CHUNK)
        .zip(raw.par_chunks(CHUNK * bpp))
        .for_each(|(dst, src)| {
            match geom.bitpix {
                8 => for (d, s) in dst.iter_mut().zip(src.iter()) {
                    *d = *s as f32;
                },
                16 if fast_u16 => for (d, s) in dst.iter_mut().zip(src.chunks_exact(2)) {
                    *d = (i16::from_be_bytes([s[0], s[1]]) as i32 + 32_768) as f32;
                },
                16 => for (d, s) in dst.iter_mut().zip(src.chunks_exact(2)) {
                    *d = i16::from_be_bytes([s[0], s[1]]) as f32;
                },
                32 => for (d, s) in dst.iter_mut().zip(src.chunks_exact(4)) {
                    *d = i32::from_be_bytes([s[0], s[1], s[2], s[3]]) as f32;
                },
                64 => for (d, s) in dst.iter_mut().zip(src.chunks_exact(8)) {
                    *d = i64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]) as f32;
                },
                -32 => for (d, s) in dst.iter_mut().zip(src.chunks_exact(4)) {
                    *d = f32::from_be_bytes([s[0], s[1], s[2], s[3]]);
                },
                -64 => for (d, s) in dst.iter_mut().zip(src.chunks_exact(8)) {
                    *d = f64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]) as f32;
                },
                // Geometry::from_header rejects anything else, so this is dead code.
                // Fill with NaN rather than panicking in a worker thread.
                _ => dst.fill(f32::NAN),
            }
            if scaled {
                for d in dst.iter_mut() {
                    *d = (bzero + bscale * (*d as f64)) as f32;
                }
            }
            if let Some(blank) = geom.blank_as_f32 {
                for d in dst.iter_mut() {
                    if *d == blank { *d = f32::NAN; }
                }
            }
        });
}

/// Min and max over finite values only. Returns (0.0, 1.0) if nothing is finite,
/// so that callers never divide by zero.
pub fn finite_min_max(data: &[f32]) -> (f32, f32) {
    let (lo, hi) = data
        .par_iter()
        .filter(|v| v.is_finite())
        .fold(|| (f32::INFINITY, f32::NEG_INFINITY),
              |(lo, hi), &v| (lo.min(v), hi.max(v)))
        .reduce(|| (f32::INFINITY, f32::NEG_INFINITY),
                |a, b| (a.0.min(b.0), a.1.max(b.1)));
    if lo.is_finite() && hi.is_finite() && hi > lo { (lo, hi) } else { (0.0, 1.0) }
}
```

Note the `par_chunks_mut(CHUNK).zip(par_chunks(CHUNK * bpp))` pairing: both are
indexed parallel iterators, so rayon keeps the two sides aligned. `chunks_exact`
guarantees each `s` has the length the array literal indexes, so no bounds check
survives optimisation and no `unwrap` is needed.

Why this is fast enough without `mmap`: the conversion pass touches every byte anyway, so the extra copy that `fs::read` performs is a small fraction of total time and is done by the kernel at memory bandwidth. Measured expectation for a 24 MP 16-bit file: read ≈ 15 ms (warm), convert ≈ 30–60 ms with 8 threads. If a benchmark shows otherwise, report the numbers and ask before changing strategy (rule 1.1).

### 5.4 Synthetic FITS generator for tests (`fits-core/src/testutil.rs`)

Expose behind a cargo feature `test-util` so the `fitsview` crate can use it in its own tests:

```rust
pub struct SyntheticSpec { pub width: usize, pub height: usize, pub channels: usize, pub bitpix: i64, pub bzero: f64, pub bscale: f64, pub extra_cards: Vec<(String, String)> }
pub fn synthetic_fits(spec: &SyntheticSpec, pixels: &[f64]) -> Vec<u8>;       // valid FITS bytes
pub fn write_synthetic(dir: &Path, name: &str, spec: &SyntheticSpec, pixels: &[f64]) -> PathBuf;
pub fn gaussian_background(width: usize, height: usize, mean: f64, sigma: f64, seed: u64) -> Vec<f64>; // deterministic PRNG, no rand crate
```

Every test that needs a file uses these. No binary fixtures are committed.

### 5.5 Test plan by module

| Module | Test type | What is covered |
|--------|-----------|-----------------|
| `header.rs` | unit + proptest | Card parsing, quoted strings with `/` inside, `END` detection, multi-block headers, missing `NAXIS` → `BadHeader`, arbitrary bytes never panic. |
| `image.rs` | unit + criterion | Every BITPIX, BZERO/BSCALE, u16 fast path equals the generic path, `finite_min_max` with NaN/inf present and with an all-NaN image, `BLANK` becomes NaN, 3-channel layout. |
| `reader.rs` | unit + proptest | Truncated, NotFits, absurd NAXIS values do not overflow or allocate wildly, primary-empty-then-extension fallback, `write_fits`→`read_fits` identity. |
| `stretch.rs` | unit | Median maps to `target_bg` within tolerance, LUT is monotonic non-decreasing, constant image does not divide by zero, all-NaN image does not panic, RGB per-channel. |
| `calib.rs` | unit | Median rejects outlier, mean for N≤2, dimension mismatch error, subtract clamps at 0, all dark/bias/scale combinations. |
| `folder.rs` | integration (tempdir) | Filters extensions, skips hidden, natural sort, sidecar round-trip. |
| `actions.rs` | integration (tempdir) | Rename rules, flag toggle persists, delete calls trash (mock via trait `FileOps` so tests don't touch the real trash). |
| `loader.rs` | unit | LRU eviction by count and bytes, stale generation dropped, prefetch order. |
| `texture.rs` | unit | Vertical flip applied exactly once, downsample factor selection, NaN maps to black. |
| `app.rs` model | unit | Keyboard actions mutate `Model` correctly: next/prev at ends, delete advances selection, flagged delete requires confirm state. |
| UI files | manual | `docs/manual-tests.md` checklist per phase. |

---

## Phase 0 — Bootstrap

**Goal:** An empty but complete workspace that builds, tests, lints, and runs in
CI on all three platforms. No application code yet. This phase exists so that
every later phase starts from a green build.

### Steps

1. Root `Cargo.toml` as a workspace:
   ```toml
   [workspace]
   members = ["crates/fits-core", "crates/fitsview"]
   resolver = "2"

   [workspace.package]
   edition = "2021"
   rust-version = "1.78"

   [profile.release]
   opt-level = 3
   lto = "fat"
   codegen-units = 1
   panic = "abort"

   # Dependencies are still built with optimisation in debug builds, otherwise
   # decoding a 24 MP image while developing takes seconds instead of milliseconds.
   [profile.dev.package."*"]
   opt-level = 3
   ```
2. `cargo new --lib crates/fits-core` and `cargo new crates/fitsview`. Add `#![forbid(unsafe_code)]` as the first line of `crates/fits-core/src/lib.rs` and `crates/fitsview/src/main.rs`.
3. Add `rust-toolchain.toml` pinning `channel = "stable"` with components `rustfmt` and `clippy`, so every machine and CI agent agrees.
4. Create `docs/manual-tests.md` with a heading per phase and nothing under them yet.
5. Write `.github/workflows/ci.yml` now, not in Phase 8. Matrix over `ubuntu-latest`, `macos-latest`, `windows-latest`, with steps:
   - `cargo fmt --all --check`
   - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
   - `cargo test --workspace --all-features`
   - `cargo build --release`
   - a guard step that fails if the string `unsafe` appears in `crates/**/*.rs`
   - on Linux only, first install `libgtk-3-dev libxkbcommon-dev libssl-dev` (needed by `rfd` and the windowing stack)
6. Add one trivial passing test in each crate so the test step is not vacuous.
7. Commit `Cargo.lock`. It is a binary crate, so the lock file belongs in version control.

### Acceptance criteria
- [x] `cargo build --workspace` and `cargo test --workspace` succeed locally.
- [x] `cargo clippy --workspace --all-targets --all-features -- -D warnings` is clean.
- [x] `cargo fmt --all --check` is clean.
- [x] The unsafe guard is present, and is itself tested against a real `unsafe`
      block and against a deleted `forbid` attribute.
- [x] `Cargo.lock` is committed.
- [ ] CI is green on Linux, macOS, and Windows. **Cannot be confirmed until the
      repository has a remote and the workflow has run at least once.**

### What Phase 0 actually produced

Built and verified locally on macOS with Rust 1.94.0:

- Workspace with `crates/fits-core` (library) and `crates/fitsview` (binary),
  both carrying `#![forbid(unsafe_code)]`.
- Six passing tests: three unit tests and one doctest in `fits-core`, two unit
  tests in `fitsview`. They are small but real, covering `block_align` rounding
  and its overflow behaviour, rather than `assert!(true)` placeholders.
- `scripts/check-unsafe.sh`, which checks both that each crate root has the
  `forbid` attribute and that the `unsafe` keyword appears nowhere in our
  sources. Prose mentioning the word in comments does not trip it.
- `.github/workflows/ci.yml` with three jobs: `checks` (fmt, clippy, unsafe
  guard, once on Linux), `test` (test and release build across all three
  operating systems), and `msrv` (checks the declared `rust-version` really
  builds).

**Known issue for Phase 2.** The declared minimum Rust version is `1.78`, which
is fine for the current dependency-free code. `egui` and `eframe` need a much
newer compiler, so the `msrv` CI job **will fail** when they are added. That is
the job doing its work. Raise `rust-version` in the root `Cargo.toml` to
whatever the new dependencies require, and say so in the commit message.

---

## Phase 1 — FITS Reader Library (`fits-core`)

**Goal:** Open any common astrophotography FITS file into a `FitsImage` as fast as possible.

### Steps

1. Work in `crates/fits-core`, created in Phase 0. The crate currently holds
   only the block-size constants and `block_align`; build the reader around
   those rather than redefining them.
2. Add dependencies: `rayon`, `thiserror`. Add `#![forbid(unsafe_code)]` to `lib.rs`. Dev-deps: `tempfile`, `criterion`, `proptest`, `approx`.
3. Implement `header.rs`:
   - Read 2880-byte blocks. Split each into 36 cards of 80 bytes.
   - For each card: keyword = trimmed bytes 0..8. If keyword is `END` stop.
     If bytes 8..10 are `= ` then value = bytes 10..80 up to the first `/` that is
     **not** inside single quotes. Trim whitespace. Strip surrounding single quotes for strings.
   - Store `(keyword, value)`. Ignore `COMMENT`, `HISTORY`, and blank keywords.
   - Return the header **and** the byte offset where data starts (next 2880 boundary after END block).
4. Implement `image.rs`:
   - Function `convert_pixels(bitpix, raw_bytes, bzero, bscale, out: &mut [f32])`.
   - Use `rayon::par_chunks` over the raw byte slice to convert in parallel.
   - Use `from_be_bytes` on fixed-size chunks. **Do not** use `byteorder`'s per-element reader in a loop; it is slower.
   - Fast path: if `bitpix == 16 && bzero == 32768.0 && bscale == 1.0` treat as `u16` directly (`(i16 as i32 + 32768) as f32`).
   - Compute `min` and `max` with a parallel reduce during the same pass, or in a second parallel pass.
5. Implement `reader.rs`:
   - `read_fits(path)`: `std::fs::read` the file (see 5.3), check magic `SIMPLE`, parse header, verify `NAXIS` is 2 or 3, compute expected byte length, error `Truncated` if file is too short, convert pixels, return `FitsImage`.
   - If primary HDU has `NAXIS == 0`, skip its (zero-length) data and parse the next HDU header (`XTENSION= 'IMAGE'`). Only one level of fallback is required.
6. Add `pub fn is_fits_path(path: &Path) -> bool` — extension check only (`.fits`, `.fit`, `.fts`, case-insensitive). Used by folder scanning.
7. Write `testutil.rs` (see 5.4) **first**, then use it in tests for every supported `BITPIX`.
8. Add `proptest` tests: (a) `header::parse` never panics on arbitrary input; (b) `convert_pixels` u16 fast path equals the generic path for random data.
9. Add a `criterion` bench in `benches/read.rs` that times `read_fits` on a 6000×4000 16-bit synthetic file written to a temp dir. Target: **< 150 ms** on a modern laptop, warm cache.
10. Run `cargo geiger -p fits-core` and confirm zero `unsafe` in the crate.

### Acceptance criteria
- [ ] `cargo test -p fits-core` passes with tests for BITPIX 8, 16 (with BZERO 32768), 32, -32, -64.
- [ ] Reading a truncated file returns `FitsError::Truncated`, not a panic.
- [ ] Non-FITS file returns `FitsError::NotFits`.
- [ ] A 3-plane (`NAXIS3=3`) file returns `channels == 3`.
- [ ] `min`/`max` are correct for the synthetic tests.
- [ ] No `unwrap()` on user data paths.
- [ ] `#![forbid(unsafe_code)]` present; `cargo geiger` reports 0 unsafe in `fits-core`.
- [ ] Property tests and criterion bench exist and run.

---

## Phase 2 — Minimal GUI: Open One File and Display It

**Goal:** A window that opens a single FITS file and shows it, fitted to the window, with zoom and pan.

### Steps

1. `cargo new crates/fitsview` (binary). Add `#![forbid(unsafe_code)]` to `main.rs`. Dependencies: `eframe`, `egui`, `rfd`, `fits-core`, `log`, `env_logger`, `anyhow`. Dev-deps: `tempfile`, `fits-core` with feature `test-util`.
2. `main.rs`: init logging, build `eframe::NativeOptions` (title "fitsview", initial size 1400×900, `vsync: true`), run `app::FitsViewApp`.
   - Accept an optional command-line argument: a file or folder path to open at startup.
3. `texture.rs`: `fn to_color_image(img: &FitsImage, mapper: &dyn Fn(f32) -> u8) -> egui::ColorImage`.
   - Map each `f32` to a `u8` using the supplied mapper. Phase 2 mapper: linear from `min..max`.
   - For `channels == 3` produce RGB, else gray.
   - Use `rayon` for the pixel loop. Build the `Vec<egui::Color32>` then `ColorImage`.
   - Upload with `ctx.load_texture(name, color_image, TextureOptions::LINEAR)`.
   - **Important for performance:** for images larger than 4096 on a side, downsample by integer factor (2× or 4×, box filter) for the display texture. Keep the full-resolution `FitsImage` in memory for stats and for later phases. A "1:1" zoom mode can re-upload a cropped full-res region later; not required now.
4. `ui/viewer.rs`: a central panel that draws the texture.
   - Fit-to-window on first show.
   - Mouse wheel = zoom about the cursor. Drag = pan. Key `F` = reset to fit. Key `1` = 100 % zoom.
   - Draw using `ui.painter().image(texture_id, rect, uv, tint)`.
5. `ui/toolbar.rs`: a top panel with buttons `Open File…`, `Open Folder…` (Phase 3 wires this), and a status label showing filename, dimensions, BITPIX, and load time in ms.
6. `app.rs`: split into a plain `Model` struct (no egui types except `Vec2`/`Rect` if needed) holding `current`, `ViewState`, toggles, toasts, and an `impl Model { fn handle(&mut self, action: Action) }` enum-driven state machine; then a thin `FitsViewApp` wrapper implementing `eframe::App::update` that translates input into `Action`s and renders `Model`. Unit-test `ViewState` zoom/pan math (zoom about cursor keeps the point under the cursor fixed; fit computes correct scale) and `Model::handle`.
7. Handle drag-and-drop of a file onto the window (`ctx.input(|i| i.raw.dropped_files)`).

### Acceptance criteria
- [ ] `cargo run -p fitsview -- path/to/file.fits` displays the image.
- [ ] `Open File…` dialog works on all three OSes (rfd).
- [ ] Zoom/pan is smooth (60 fps) on a 24-megapixel image.
- [ ] Load-time label shows a real measured value.
- [ ] Drag-and-drop works.
- [ ] Image is the right way up: a synthetic file with a bright first FITS row shows that row at the **bottom** of the window (see the row-order note in section 4).
- [ ] Non-finite pixels render black instead of blanking the image.
- [ ] Unit tests for `ViewState` (fit, zoom-about-cursor, pan) and `texture::downsample_factor` pass.
- [ ] `docs/manual-tests.md` has a Phase 2 checklist, ticked.

---

## Phase 3 — Folder Browsing

**Goal:** Open a folder, list only FITS files, navigate between them instantly.

### Steps

1. `folder.rs`:
   ```rust
   pub struct FileEntry { pub path: PathBuf, pub name: String, pub size: u64, pub flagged: bool }
   pub struct Folder { pub dir: PathBuf, pub files: Vec<FileEntry>, pub selected: Option<usize> }
   pub fn scan_folder(dir: &Path) -> anyhow::Result<Folder>;
   ```
   - Non-recursive. Use `is_fits_path` to filter. Sort by name (natural sort: `light_2.fits` before `light_10.fits`; use the `natord` crate or write a small comparator).
   - Skip hidden files (name starts with `.`).
2. `loader.rs`: background loading with a cache.
   - Spawn one worker thread. Channel `Request { path, generation }` → worker → `Response { path, generation, result: Result<Arc<FitsImage>, FitsError> }`.
   - Cache: `HashMap<PathBuf, Arc<FitsImage>>` bounded to N entries (default 8, LRU by simple `VecDeque` order). Memory guard: also cap by total bytes (default 2 GB).
   - **Prefetch:** when the user selects index `i`, request `i`, then `i+1`, then `i-1` if not cached. This makes "next image" feel instant.
   - The UI calls `loader.poll()` every frame (non-blocking `try_recv`) and calls `ctx.request_repaint()` when something arrived.
   - Texture creation also happens once per image and is cached alongside (`HashMap<PathBuf, TextureHandle>`), same bound.
3. `ui/filelist.rs`: a left side panel with a scrollable list of file names. Clicking selects. Show a small flag icon column (Phase 4) and file size.
4. Keyboard: `→` / `Space` / `PageDown` = next, `←` / `PageUp` = previous, `Home` / `End` = first/last. Selection wraps? **No.** Stop at ends.
5. `Open Folder…` toolbar button and drag-and-drop of a folder. Opening a single file via `Open File…` should also load its parent folder and select that file.
6. Show `"3 / 142"` counter in the toolbar.
7. Watch for external changes: **not required**. Add a `Rescan` button (key `F5`) instead.

### Acceptance criteria
- [ ] Folder with mixed files shows only `.fits/.fit/.fts`.
- [ ] Pressing `→` repeatedly through 50 files of 24 MP never stalls the UI thread (measure: frame time stays under 20 ms; loading happens off-thread).
- [ ] Memory stays bounded (verify with a 200-file folder).
- [ ] Natural sort order is correct (unit test with `light_2`, `light_10`, `Light_1`).
- [ ] `loader::Cache` unit tests: eviction by count, eviction by bytes, stale generation dropped, prefetch order `i, i+1, i-1`.
- [ ] `scan_folder` integration test on a tempdir with mixed files.

---

## Phase 4 — File Management: Delete, Rename, Flag

**Goal:** Fast culling workflow.

### Steps

1. **Flag to keep (d):**
   - Toolbar toggle button `★ Keep` and key `K` toggles `flagged` for the selected file.
   - Show ★ in the file list row and in the viewer overlay corner.
   - Persist flags in a sidecar file `<folder>/.fitsview.json` containing `{"flagged": ["name1.fits", ...]}`. Load it on `scan_folder`, write it on every change (small file, write atomically: write to temp then rename).
2. **Delete (c1):**
   - Toolbar button `🗑 Delete` and key `Delete` (also `Backspace` on macOS).
   - Behaviour: move to OS trash using the `trash` crate. Never hard-delete.
   - If the file is **not** flagged: delete immediately, no dialog. Selection moves to the next file (or previous if it was last). Show a 3-second toast "Deleted `name` — Ctrl+Z to undo" (undo = `trash::os_limited::restore_all` where supported; if unsupported on the platform, hide the undo hint).
   - If the file **is** flagged: show a modal dialog: "`name` is flagged to keep. Delete anyway?" with `Cancel` (default, `Esc`) and `Delete` (`Enter` must NOT trigger delete; user must click or press `Shift+Delete`).
   - Add a toolbar toggle `Confirm every delete` (off by default) that forces the dialog for unflagged files too.
3. **Rename (c2):**
   - Key `F2` or toolbar `Rename` opens an inline text edit in the file list row (or a small modal if inline is hard) pre-filled with the name, with the stem selected and the extension not selected.
   - `Enter` commits, `Esc` cancels. Reject: empty name, name containing path separators, name that already exists (show red hint text). Keep the original extension if the user removed it.
   - Use `std::fs::rename`. Update the entry in place, keep selection, keep flag state (update sidecar).
4. Refactor all three operations into `actions.rs` with functions that take `&mut Folder` and a `&dyn FileOps` and return `Result<ActionOutcome>`. Define
   ```rust
   pub trait FileOps { fn trash(&self, p: &Path) -> anyhow::Result<()>; fn rename(&self, from: &Path, to: &Path) -> anyhow::Result<()>; fn exists(&self, p: &Path) -> bool; }
   pub struct RealFileOps; // uses trash::delete and std::fs::rename
   ```
   In tests use a `RecordingFileOps` that records calls and simulates the filesystem in a tempdir, so tests never touch the real OS trash.
5. Add a `Help` overlay (key `?` or `H`) listing all shortcuts.

### Keyboard shortcut summary (keep this table in the app's Help overlay)

| Key | Action |
|-----|--------|
| `→` `Space` `PgDn` | Next file |
| `←` `PgUp` | Previous file |
| `Home` / `End` | First / last file |
| `K` | Toggle keep flag |
| `Delete` / `Backspace` | Delete (to trash) |
| `Shift+Delete` | Confirm delete of flagged file |
| `F2` | Rename |
| `Ctrl+Z` | Undo last delete (where supported) |
| `S` | Toggle stretch (Phase 5) |
| `F` / `1` | Fit / 100 % zoom |
| `F5` | Rescan folder |
| `?` | Help |

### Acceptance criteria
- [ ] Deleting an unflagged file moves it to trash and advances selection with no dialog.
- [ ] Deleting a flagged file always shows the confirmation; `Enter` does not confirm.
- [ ] Flags survive app restart (sidecar file).
- [ ] Rename rejects duplicates and bad names; extension is preserved.
- [ ] `actions.rs` unit tests pass on Linux, macOS, Windows in CI.

---

## Phase 5 — Astro Stretch

**Goal:** One button toggles a standard auto-stretch on every displayed image.

### Algorithm: Midtone Transfer Function (MTF) auto-stretch (the PixInsight "STF" convention)

Implement in `fits-core/src/stretch.rs`:

```rust
pub struct StretchParams { pub shadows_clip: f32, pub target_bg: f32 } // defaults: -2.8, 0.25
pub struct Stretch { pub shadows: f32, pub midtones: f32, pub highlights: f32 }

/// One Stretch per channel (len 1 for mono, 3 for RGB).
pub fn compute_stretch(img: &FitsImage, p: &StretchParams) -> Vec<Stretch>;

/// 65536-entry lookup table. Boxed so it is not copied on the stack.
pub fn build_lut(s: &Stretch) -> Box<[u8; 65536]>;
```

**The midtone transfer function.** Fix the argument order once and never vary it:

```rust
/// m is the midtone balance in (0,1); x is the input in [0,1].
fn mtf(m: f32, x: f32) -> f32 {
    if x <= 0.0 { return 0.0; }
    if x >= 1.0 { return 1.0; }
    if m == 0.5 { return x; }
    ((m - 1.0) * x) / (((2.0 * m - 1.0) * x) - m)
}
```

`mtf` has a property this algorithm depends on: if `t = mtf(m, x)` then
`mtf(t, x) = m`. The midtone and the output swap roles. That is why step 6 can
call `mtf` itself to *solve* for the midtone that puts the background where we
want it, instead of inverting the function by hand.

Steps to compute, per channel:

1. Normalise pixel values to `[0,1]` using the image `min`/`max`. Skip non-finite values entirely.
2. Compute the **median** `med` and the **MAD** (median absolute deviation) of the normalised samples.
   - Subsample: take every k-th pixel so at most ~1 million samples are used. Use `select_nth_unstable` for the median, which is O(n); never a full sort.
   - `sigma = 1.4826 * MAD` (this rescales MAD to a standard-deviation equivalent).
3. `shadows = clamp(med + shadows_clip * sigma, 0.0, 1.0)`, with `shadows_clip = -2.8`. Because `shadows_clip` is negative this lands below the median, at roughly the noise floor.
4. `highlights = 1.0`.
5. Rescale the median into the shadows-to-highlights window **before** solving:
   `x0 = (med - shadows) / (highlights - shadows)`.
6. `midtones = mtf(target_bg, x0)`. Clamp the result into `(0.001, 0.999)`.
7. Per pixel: `y = mtf(midtones, clamp((x - shadows) / (highlights - shadows), 0.0, 1.0))`, then `u8 = (y * 255.0).round()`.

Step 5 is the step that is easy to skip, and skipping it is silent. If you solve
for the midtone using `med - shadows` instead of `x0`, the background comes out
at about `0.28` instead of `0.25` for the example below: too bright, but not
obviously broken by eye. The unit test in the next section is what catches it.

**Worked example to test against.** Background median `0.20`, sigma `0.02`,
defaults `shadows_clip = -2.8`, `target_bg = 0.25`:

| Quantity | Formula | Value |
|----------|---------|-------|
| `shadows` | `0.20 + (-2.8 × 0.02)` | `0.144` |
| `highlights` | fixed | `1.0` |
| `x0` | `(0.20 - 0.144) / (1.0 - 0.144)` | `0.065421` |
| `midtones` | `mtf(0.25, 0.065421)` | `0.173554` |
| median through the full pipeline | `mtf(0.173554, 0.065421)` | `0.250000` → `64` as `u8` |

These values are verified. Assert the last two rows in a unit test to 6 decimal
places. If the median comes back at about `0.282`, step 5 was skipped. If it
comes back at some unrelated value, the argument order of `mtf` is reversed.

**Degenerate inputs that must not panic:** a constant image (`MAD = 0`, so
`sigma = 0` and `med - shadows = 0`; fall back to a linear ramp), an all-NaN
image (return an identity stretch), and `min == max` (already guarded by
`finite_min_max` returning `(0.0, 1.0)`).

**Performance:** do not evaluate `mtf` per pixel. Build the 65536-entry `u8` LUT
once per image (index = normalised value quantised to 16 bits), then map pixels
through it with `rayon`. That turns the stretch into one multiply, one cast and
one array index per pixel, so it costs about the same as the linear display path.
Cache the LUT next to the image in the loader cache, keyed by the stretch
parameters, so toggling back and forth does not recompute it.

### UI

1. Toolbar toggle `Stretch` (key `S`). Global setting: applies to every image shown, and is remembered across sessions (store in `eframe` persistence via `Storage`).
2. When toggled, invalidate the texture cache and rebuild the current texture (and prefetched ones lazily).
3. Optional (small): a collapsible "Stretch settings" with two sliders for `shadows_clip` (-5 … 0) and `target_bg` (0.05 … 0.5). Reset button.
4. Stretch parameters are computed **per image** (auto), not shared.

### Acceptance criteria
- [ ] Unit test: synthetic Gaussian background, median maps to `target_bg * 255` ±3.
- [ ] Unit test: the worked example table above reproduces to 6 decimal places.
- [ ] Unit test: LUT is monotonic non-decreasing across all 65536 entries.
- [ ] Unit test: constant image, all-NaN image, and single-pixel image do not panic.
- [ ] Toggling stretch on a 24 MP image re-renders in under 100 ms once the LUT is cached.
- [ ] Setting persists across restart.

---

## Phase 6 — Dark-Frame Calibration

**Goal:** Select one or more dark frames, build a master dark, subtract it from every light frame in the folder (for display, and optionally write calibrated files).

### Concepts
- A **dark** is an exposure with the shutter closed, same exposure time and temperature as the lights. Subtracting it removes thermal signal and hot pixels.
- **Master dark** = pixel-wise **median** of N darks (median rejects cosmic rays). If N == 1 use it directly.
- Calibrated light = `light - master_dark`, clamped at 0 (or keep negatives and let the stretch handle it; **clamp at 0** for simplicity).
- Darks must match light dimensions exactly. Ideally match `EXPTIME` (±5 %) and `CCD-TEMP`/`SET-TEMP` — warn but do not block if they differ.

### Implementation in `fits-core/src/calib.rs`

```rust
pub struct MasterFrame { pub width: usize, pub height: usize, pub channels: usize, pub data: Vec<f32>, pub source_count: usize, pub exptime: Option<f64> }
pub fn build_master_median(frames: &[Arc<FitsImage>]) -> Result<MasterFrame, CalibError>;
pub fn subtract_dark(light: &FitsImage, dark: &MasterFrame) -> Result<FitsImage, CalibError>;
pub fn write_fits(path: &Path, img: &FitsImage) -> Result<(), FitsError>; // BITPIX=-32 output, copy header, add HISTORY card
```

- `build_master_median`: for each pixel index, gather N values, `select_nth_unstable` for the median. Parallelise over row chunks with `rayon`. For N ≤ 2 use mean.
- Keep the master in memory as `Arc<MasterFrame>`; also allow **saving** it as `master_dark.fits` (via `write_fits`) and **loading** an existing master (any FITS file can be used as a master).
- `write_fits`: write header cards (`SIMPLE`, `BITPIX=-32`, `NAXIS`, `NAXIS1/2/3`, copy other original cards except `BZERO/BSCALE/BITPIX/NAXIS*`, add `HISTORY fitsview: dark subtracted (N frames)`), pad to 2880, write big-endian f32 data, pad to 2880.

### UI

1. New collapsible right-side panel `Calibration`.
2. `Add darks…` (multi-file dialog) → shows list with count, dimension, EXPTIME; `Build master` button; `Load master…`; `Save master…`; `Clear`.
3. Toggle `Apply dark` (key `D`). When on, the display pipeline becomes: load → subtract master dark → (stretch) → texture. Cache calibrated images separately (`HashMap<PathBuf, Arc<FitsImage>>`) with the same size bound.
4. Mismatch (dimensions) → toggle is disabled and a red message explains why.
5. `Export calibrated…` → choose output folder → writes `<name>_cal.fits` for every file in the folder, using a background thread with a progress bar and cancel button. Never overwrites originals.

### Acceptance criteria
- [ ] Unit test: master median of 3 synthetic frames with one outlier pixel rejects the outlier.
- [ ] Unit test: `light - dark` for known values, clamped at 0.
- [ ] Round-trip test: `write_fits` then `read_fits` returns identical data.
- [ ] Applying dark on a 24 MP image adds < 50 ms per image (parallel subtract).
- [ ] Export runs off the UI thread, is cancellable, and never touches originals.

---

## Phase 7 — Bias-Frame Calibration

**Goal:** Same workflow as Phase 6 for bias frames, and correct ordering when both are used.

### Concepts
- A **bias** is a zero-length (shortest possible) exposure. It captures the sensor read offset.
- **Master bias** = pixel-wise median of N biases (reuse `build_master_median`).
- If you have a **master dark that already contains bias** (which is the normal case for an un-scaled dark subtraction), you should **not** also subtract bias. So the rules are:
  - Bias only: `light - master_bias`.
  - Dark only: `light - master_dark`.
  - Both: `light - master_dark` (dark already includes bias). Show an info message: "Bias not subtracted separately because a dark is applied." Only if the user enables `Scale dark` (optional feature, see below) does bias get used: `light - bias - k * (dark - bias)`.
- Optional `Scale dark` (dark optimisation): `k = EXPTIME_light / EXPTIME_dark`. Implement only if `EXPTIME` is available in both headers; otherwise disabled.

### Implementation

1. Add `pub fn subtract_bias(light, bias) -> FitsImage` (identical to `subtract_dark`; factor out a shared `subtract_frame`).
2. Add `pub fn calibrate(light, dark: Option<&MasterFrame>, bias: Option<&MasterFrame>, scale_dark: bool) -> Result<FitsImage>` implementing the rules above. This is the single entry point the UI calls.
3. UI: in the `Calibration` panel add a `Bias` section mirroring `Darks` (`Add biases…`, `Build master`, `Load/Save master`, `Apply bias` toggle, key `B`), plus `Scale dark` checkbox.
4. Export uses `calibrate` and writes `HISTORY` cards describing what was applied.
5. Persist last-used master dark/bias paths per folder in the `.fitsview.json` sidecar so reopening a folder restores the calibration setup.

### Acceptance criteria
- [ ] Unit tests for all four combinations (none / dark / bias / both) and for `scale_dark`.
- [ ] Info message appears when both are enabled without `Scale dark`.
- [ ] Sidecar restores calibration paths on reopen.

---

## Phase 8 — Packaging, CI, and Polish

1. **CI** was set up in Phase 0. Extend it here to upload the release binary as a per-OS build artifact, and add a tag-triggered job that attaches those binaries to a GitHub release.
2. **Release profile** was set in Phase 0. Verify `cargo build --release` still produces a single self-contained binary per platform.
3. **App icon** and window title. On macOS, produce a `.app` bundle with `cargo-bundle`; on Windows set the icon via `winres`. Optional.
4. **Settings persistence**: window size/position, stretch toggle, stretch params, last folder — via `eframe` `Storage`.
5. **Error handling**: every failure surfaces as a non-blocking toast; never a panic. Add `std::panic::set_hook` that logs and shows a message box (`rfd::MessageDialog`) before exit.
6. **Header viewer**: key `I` toggles a panel listing all header cards of the current file. Cheap and very useful.
7. **Histogram**: small histogram widget under the viewer (256 bins, computed at load time on the subsample). Optional.

---

## 9. Performance Checklist (apply throughout)

- Read files with a single `std::fs::read`; do not use `BufReader` per-element reads, and do not use `mmap` (unsafe).
- Convert pixels in parallel with `rayon`; use `from_be_bytes` on `[u8; N]` chunks.
- Never block the UI thread on I/O. All loading/calibration/export runs on worker threads and communicates via channels.
- Prefetch neighbours; cache decoded images and textures with a bounded LRU.
- Downsample display textures for very large images; keep full-res data in memory.
- Stretch via a 64 K LUT, never a per-pixel `powf`/division.
- Compute statistics (median/MAD/histogram) on a ≤ 1 M sample, never the full image.
- Hoist the BITPIX match out of the pixel loop (section 5.3). A per-pixel branch costs roughly 30 %.
- Keep `[profile.dev.package."*"] opt-level = 3` so debug builds decode images at usable speed.
- Build in `--release` for any timing measurement. Log load/convert/texture times with `log::debug!`.
- Profile before optimising further: `cargo flamegraph` on Linux/macOS.

---

## 10. Definition of Done (whole project)

- [ ] All phase acceptance criteria checked.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean on all three OSes.
- [ ] `cargo test --workspace` green in CI on all three OSes.
- [ ] Zero `unsafe` in `crates/`: both crates carry `#![forbid(unsafe_code)]` and the CI guard step passes.
- [ ] Every module in the test plan table (5.5) has the listed tests.
- [ ] Images display right way up, and files containing NaN pixels render correctly.
- [ ] Manual test with real files from at least two capture programs (e.g. N.I.N.A. and ASIAIR/ZWO) — both 16-bit `BZERO=32768` and 32-bit float.
- [ ] A 24 MP file opens and displays in well under one second on a warm cache.
- [ ] Delete / rename / flag / stretch / dark / bias all work from keyboard alone.
