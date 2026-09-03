# fitsview

A fast desktop viewer for the FITS images astrophotography produces. Written in
Rust, it runs on Linux, macOS and Windows, and it is built for the job that
follows a night's capture: going through a folder of frames, throwing out the
ones ruined by cloud or tracking, calibrating the rest, and seeing what you
actually caught.

---

## Features

**Opening images.** Reads every common FITS format an astronomy camera writes:
8, 16, 32 and 64-bit integers and 32 and 64-bit floats, signed or unsigned, mono
or colour. A 24-megapixel frame decodes in about **5 ms**. Images are read on a
background thread and neighbours are fetched before you ask for them, so holding
down the arrow key through a folder never makes the window wait.

**Going through a folder.** Opens a folder and lists only the FITS files in it,
in the order a human would expect: `light_2` before `light_10`. Step through with
the arrow keys, Space, or Page Up and Down. The file list collapses to the edge
when you want the image to fill the screen.

**Culling.** Mark a frame to keep with `K`, delete one with `Delete`. Deleting
always moves the file to the system trash, never destroys it, and a frame marked
to keep always asks first. Rename with `F2`. Keep marks are stored beside the
images, so they survive restarting and travel with the folder if you copy it.

**Seeing the image.** A raw frame is almost black; `S` applies the standard
midtone stretch that puts the sky background at a sensible brightness and brings
the faint signal into view. Zoom with the wheel about the pointer, drag to pan,
`F` to fit, `1` for actual size.

**Calibration.** Combine dark frames into a master, using the median so a cosmic
ray on one frame is discarded rather than smeared across every result, and
subtract it with `D`. Do the same with flats to remove vignetting and dust
shadows, applied with `Shift+F`. Both can be saved and reused, and each folder
remembers which were used with it. Export writes calibrated copies of a whole
folder in the background, never touching the originals.

**One-shot colour.** A raw frame from a colour camera is a mosaic behind a grid
of filters and displays as grey. `B` reconstructs the colour, automatically when
the file records which filter pattern it used. Exports stay as mosaics, because
that is what a stacker wants.

**Knowing what you are looking at.** The metadata panel shows the FITS header,
with the keywords that identify a frame pinned to the top and a filter for
finding the rest.

**Throughout.** Everything is reachable from the keyboard; press `?` for the
list. No `unsafe` code anywhere in the project, enforced by the compiler. Tested
by 498 automated tests that run on all three operating systems.

---

## Building and Running

`fitsview` is one Rust workspace with no code generation and no C dependencies
to compile, so on every platform the build is `cargo build --release`. What
differs is only what has to be installed first.

The macOS and Windows instructions below have been run; the Linux package lists
have not, and say so where they are uncertain.

**Rust.** Any platform needs a stable toolchain of **1.92 or newer**, which is
what `egui` requires. `rust-toolchain.toml` names the stable channel, so
`rustup` fetches the right one automatically the first time you build. Install
it from [rustup.rs](https://rustup.rs) if you have none.

### macOS

Only the Apple command line tools, for the linker:

```bash
xcode-select --install
```

Then:

```bash
cargo build --release
./target/release/fitsview
```

The binary is built for the machine that built it, so an Apple silicon Mac
produces an `arm64` binary that will not run on an Intel Mac. For one binary
that runs on both, build each and join them:

```bash
rustup target add x86_64-apple-darwin aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin
lipo -create -output fitsview \
  target/x86_64-apple-darwin/release/fitsview \
  target/aarch64-apple-darwin/release/fitsview
```

That produces one binary of about 23 MB carrying both architectures, against
10 MB for a single one.

The binary carries only the ad-hoc signature the linker applies. That is fine on
the machine that built it, but macOS will refuse to open it after it has been
copied or downloaded until you right-click and choose Open, or clear the
quarantine flag:

```bash
xattr -d com.apple.quarantine ./fitsview
```

Signing it properly needs an Apple developer certificate. There is no `.app`
bundle, so the binary runs from a terminal rather than from Launchpad.

### Linux

Building needs a C linker and the Wayland development files; the graphics and
X11 libraries are opened at runtime rather than linked, so they are needed to
run but not to compile.

On Debian and Ubuntu:

```bash
sudo apt-get install build-essential pkg-config libwayland-dev libxkbcommon-dev
cargo build --release
./target/release/fitsview
```

The equivalents elsewhere, which are the same libraries under different names:

| Distribution | Packages |
|--------------|----------|
| Fedora | `gcc pkgconf-pkg-config wayland-devel libxkbcommon-devel` |
| Arch | `base-devel pkgconf wayland libxkbcommon` |
| openSUSE | `gcc pkg-config wayland-devel libxkbcommon-devel` |

At **run** time you also need a working graphics driver, which on most systems
means Mesa, and `xdg-desktop-portal` with a backend for your desktop. The file
dialogs go through the portal rather than through GTK, so a portal that is not
installed shows up as the Open buttons doing nothing.

**On what is verified:** the continuous integration job builds on
`ubuntu-latest` with `libgtk-3-dev libxkbcommon-dev libssl-dev`, which is the
list this plan started with. Inspecting the dependency tree afterwards showed
neither GTK nor OpenSSL is actually reached: there is no `gtk-sys` and no
`openssl-sys` in it. The shorter list above is what the tree says is needed, but
it has not been tried on a clean machine, and GitHub's runner images come with
many development packages already present. If the short list fails for you, the
CI list is the one known to work.

### Windows

Rust needs the Microsoft linker, which comes with the Visual Studio Build Tools.
Install them with the **Desktop development with C++** workload selected, from
[visualstudio.microsoft.com/downloads](https://visualstudio.microsoft.com/downloads/),
then:

```powershell
cargo build --release
.\target\release\fitsview.exe
```

Nothing else is required: the file dialogs use the native Windows ones, and the
graphics go through Direct3D, which is part of the system.

### Running it

Run it from the build directory:

```bash
./target/release/fitsview path/to/folder
```

Or put it on your `PATH`, which is what the rest of this section assumes:

```bash
cargo install --path crates/fitsview
```

```bash
fitsview                      # reopens the folder from last time
fitsview path/to/image.fits   # opens one image, and browses its folder
fitsview path/to/folder       # browses a folder
fitsview --help               # every keyboard shortcut
```

A raw astronomical frame looks almost black until it is stretched, so press `S`
first if a newly opened image appears empty.

### Building the sample files

Useful for trying the application without a capture session to hand, and needed
by the manual checklist in `docs/manual-tests.md`:

```bash
cargo run --release --package fits-core --all-features --example make-sample -- /tmp/fitsview-samples
```

### Running the tests

```bash
cargo test --workspace --all-features
```

One test is excluded by default, because it moves a real file to the trash of
whoever runs it. Run it deliberately when changing deletion, on each platform:

```bash
cargo test -p fitsview --all-features -- --ignored real_delete
```

Benchmarks, which is where the timings quoted throughout this document come
from:

```bash
cargo bench --package fits-core --all-features
```

Before pushing, run everything continuous integration runs, so a failure does
not cost a round trip. See "Continuous integration" in
[phasedbuild.md](phasedbuild.md) for why that matters on a private repository:

```bash
./scripts/check.sh
```

That covers formatting, lints, the tests, the unsafe guard, and, when the
toolchain and target for them are installed, a build at the minimum supported
Rust version and a compile for Windows.

**Use the script rather than assembling the commands by hand.** Summarising
`cargo test` by adding up the "N passed" numbers looks like it works and
silently ignores failures; a broken test survived two phases of this project
that way. Exit status is the only summary worth trusting.

You can also compile for another platform without one to hand, which is how a
Windows-only build failure was caught during development:

```bash
rustup target add x86_64-pc-windows-msvc
cargo check --target x86_64-pc-windows-msvc --workspace --all-features
```

---

## How it was built

`fitsview` was built in ten phases, each with its own acceptance criteria and a
write-up of what it produced and what went wrong along the way. That plan, along
with the architecture, the reasoning behind each library choice, a primer on the
FITS format, and the per-module test plan, is in
**[phasedbuild.md](phasedbuild.md)**.

It is worth reading before changing anything: several of the bugs found during
development were subtle, silent, and are recorded there with the tests that now
prevent them.
