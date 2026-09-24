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
in the order a human would expect: `light_2` before `light_10`. Subfolders are
searched two levels down, which is the way captures are filed — target, then
date, then filter — so opening a target gathers every night of it. Zoom and pan
stay put as you step between frames, so comparing the same corner of each is a
matter of holding an arrow key. Step through with
the arrow keys, Space, or Page Up and Down. The file list collapses to the edge
when you want the image to fill the screen.

**During a session.** Leave the folder open while the camera works and each new
frame joins the list a few seconds after it is saved, is measured as it
arrives, and is pointed out if it stands out from the rest — the sky brightening
as cloud comes over, the stars swelling as focus slips. Sitting on the newest
frame follows the session, moving on to each one as it lands. A frame caught
halfway through being written says so and opens when it is finished, rather
than being reported as damaged.

**Culling.** Mark a frame to keep with `K`, delete one with `Delete`. Deleting
always moves the file to the system trash, never destroys it, and a frame marked
to keep always asks first. A folder on a volume that cannot be written to says
so in the status line and greys out the buttons that would fail. Rename with `F2`. Keep marks are stored beside the
images, so they survive restarting and travel with the folder if you copy it.
The list can be ordered by sky background, sharpness, star width or roundness
instead of by name, which brings the frames worth throwing out to one end.
Background and sharpness are taken as each frame is viewed, or for the whole
folder with **Measure**; width and roundness come from **Measure stars**, a
separate pass because finding stars in every frame is a hundred times the work
of sampling one. Every figure is remembered beside the frames, so a night is
measured once: opening it again, or opening the target it belongs to, brings
the figures straight back, and only a frame changed since is measured again.

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

Reconstructed colour looks green, and that is the sensor telling the truth:
twice as many green pixels, a higher green response, and light pollution
weighted towards green. **Neutral background** stretches each colour separately
so the sky comes out grey, which is how the colour of a frame is judged before
stacking. It weakens real colour along with the cast, so it is a check rather
than a way to view.

**Judging a frame.** `Shift+S` finds the stars and measures them: how many, how
wide (the full width at half maximum, in pixels and in arcseconds where the
header gives the focal length and pixel size) and how round. That is what
tells you whether a frame is in focus and whether the mount tracked, and it is
the measurement to cull on. Each star is circled on the image, so a bad
detection is obvious rather than hidden inside a number. It takes about 16 ms on
a 24-megapixel frame, runs in the background, and is off until asked for.

**Stacking.** **Stack folder** combines a night into one image per filter,
lining the frames up by their stars, so the signal adds while the noise adds
only with its root. Each frame is laid exactly where it belongs, to a fraction
of a pixel, by the Lanczos kernel — neither rounded to the nearest pixel, which
doubles stars slightly, nor blended into place, which blurs them — with a guard
against the dark rings that kernel digs around stars too sharp for the pixels. Frames taken on the other side of a meridian flip are turned
to match — including the fraction of a degree the mount does not come back by,
which would otherwise leave the middle of the frame lining up and the edges
smeared. Frames are brought to a common sky before they are added, so a frame
taken as the moon rose does not lift the result.

Each frame counts for what it is worth rather than for one: a quiet, sharp frame
counts for more than a noisy, soft one, which on a night whose sky brightens
fivefold is worth about forty per cent of the noise in the result. Satellite
trails and cosmic rays are left out. Each sample is judged against the other
frames with itself set aside, in terms of its own frame's noise, so a frame
taken under a brighter sky loses no more of its ordinary samples than a quiet
one does — on pure noise, the 0.3 per cent that chance says. Both can be turned
off.

The calibration and colour settings in force are applied on the way in, and the
stacks are written beside the frames. Ten 61-megapixel frames take about four
seconds, or ten with outliers rejected, which reads every frame a second time
and judges every sample of it against the others. Rejecting against colour
frames that size needs about 8.5 GB of memory at its peak, however many frames
there are; on a machine without that to spare it stacks a colour at a time
instead, in about 5.5 GB, reading every frame three times as often, to exactly
the same result.

**Knowing what you are looking at.** The metadata panel shows the FITS header,
with the keywords that identify a frame pinned to the top and a filter for
finding the rest, along with the sky background, the noise, and the star
figures. The histogram shows where the stretch is putting things.

**Throughout.** Everything is reachable from the keyboard; press `?` for the
list. No `unsafe` code anywhere in the project, enforced by the compiler. Tested
by 787 automated tests, run on macOS and on Linux before every release; the
Windows build is compiled on every check and tested by hand.

---

## Installing a Release Build

Every tagged release carries a ready-built binary for each platform on its
[releases page](https://github.com/dburman/fitsview/releases). Take the one that
matches your machine. There is no installer and no runtime to install
separately: the download is a single executable.

| File | For |
|------|-----|
| `fitsview-macos-arm64.tar.gz` | Apple silicon Macs (M1 and later) |
| `fitsview-linux-x86_64.tar.gz` | 64-bit Intel and AMD Linux |
| `fitsview-windows-x86_64.zip` | 64-bit Windows |

The Mac build is Apple silicon only. An Intel Mac has to build it from source;
see [Building and Running](#building-and-running) below.

### macOS

```bash
tar xzf fitsview-macos-arm64.tar.gz
xattr -d com.apple.quarantine ./fitsview
./fitsview
```

The second line matters. The binary carries only the signature the linker
applies, so macOS quarantines anything downloaded and refuses to open it,
usually with a message about the developer not being verified. Clearing the
quarantine flag is what that dialog's *Open Anyway* button does. Right-clicking
the binary and choosing **Open** works too.

To keep it to hand, move it somewhere on your `PATH`:

```bash
sudo mv fitsview /usr/local/bin/
```

There is no `.app` bundle, so it starts from a terminal rather than from
Launchpad or Spotlight.

### Linux

```bash
tar xzf fitsview-linux-x86_64.tar.gz
chmod +x fitsview
./fitsview
```

The binary needs a working graphics driver, which on most systems means Mesa,
and `xdg-desktop-portal` with a backend for your desktop — the file dialogs go
through the portal, so a missing one shows up as the Open buttons doing
nothing. Both are present on an ordinary desktop install. To keep it to hand:

```bash
sudo mv fitsview /usr/local/bin/
```

### Windows

Unzip the archive and run `fitsview.exe`. SmartScreen will warn that the
publisher is unknown, because the binary is not signed with a certificate;
choose **More info** and then **Run anyway**. Nothing else is needed — the
graphics go through Direct3D and the file dialogs are the system's own.

### Checking what you have

```bash
fitsview --version
```

Then point it at a folder:

```bash
fitsview path/to/folder
```

If a newly opened image looks black, that is a raw astronomical frame doing what
raw astronomical frames do. Press `S`.

---

## Building and Running

`fitsview` is one Rust workspace with no code generation and no C dependencies
to compile, so on every platform the build is `cargo build --release`. What
differs is only what has to be installed first.

The macOS and Windows instructions below have been run; the Linux package lists
have not, and say so where they are uncertain.

**Rust.** Any platform needs a stable toolchain of **1.95 or newer**, which is
what `sysinfo` requires. `rust-toolchain.toml` names the stable channel, so
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
produces an `arm64` binary that will not run on an Intel Mac. Releases carry
the Apple silicon build alone: this application wants a machine that can hold a
61-megapixel frame in memory and stretch it while you pan, and the Intel Macs
still in service are not that. On one of them, build from source with the
command above and you will get a binary for it.

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

**On external drives.** If the status line says *Read-only volume* and deleting
is greyed out, the drive is mounted read-only and nothing can change files on
it. On macOS the usual cause is a drive formatted for Windows: macOS reads NTFS
but cannot write to it, whatever the permissions say. Copy the folder to the
Mac, or reformat the drive as **exFAT**, which both systems can write. Images
still open and display normally either way.

### Checking detection against real frames

Synthetic frames cannot say whether a detection threshold is right: they have
stars hundreds of deviations above a flat background, so any threshold looks
correct on them. `star-probe` runs detection over a folder of real
sub-exposures and prints what it made of each one:

```bash
cargo run --release -p fits-core --all-features --example star-probe -- path/to/folder
```

Read the columns for a sequence that should be alike. Noise far larger than it
should be means something structural is being read as noise; a frame taken with
the cover on reporting thousands of stars means the rejections are not working;
counts that jump about between consecutive frames mean the measurement is
describing the algorithm rather than the sky.

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
Rust version and a compile for Windows. With Docker running it also runs the
whole suite again on Linux, in a container, which on its own is:

```bash
./scripts/test-linux.sh
```

The first run downloads a Rust image and builds from nothing, about three
minutes; later ones take about thirty seconds.

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

`fitsview` was built in twenty-three phases, each with its own acceptance criteria
and a write-up of what it produced and what went wrong along the way. That plan,
along with the architecture, the reasoning behind each library choice, a primer
on the FITS format, and the per-module test plan, is in
**[phasedbuild.md](phasedbuild.md)**.

It is worth reading before changing anything: several of the bugs found during
development were subtle, silent, and are recorded there with the tests that now
prevent them.
