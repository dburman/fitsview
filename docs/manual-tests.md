# Manual test checklist

Some acceptance criteria in `README.md` cannot be asserted in code: whether a
dialog appears, whether an image looks right way up, whether dragging feels
smooth. Those live here.

Run the list for a phase before marking that phase done. Record the date, the
platform and the result. Anything that can move into an automated test should.

Legend: `[ ]` not run, `[x]` passed, `[!]` failed (open an issue and link it).

---

## Phase 0 — Bootstrap

No user-facing behaviour yet. Everything in Phase 0 is checked automatically by
CI, so this section is intentionally empty.

---

## Phase 1 — FITS reader

Covered by 77 automated tests, including property tests that feed the parser
arbitrary bytes. The one thing synthetic files cannot prove is that real
cameras write what the standard says they write, so this stays manual.

- [ ] Open a real 16-bit file from a capture program and confirm the reported
      dimensions and BITPIX match what the capture program shows. Deferred
      until Phase 2, when there is a user interface to read them from.

---

## Phase 2 — Minimal viewer

Generate the sample files first:

```
cargo run --release --package fits-core --all-features --example make-sample -- /tmp/fitsview-samples
```

Then open the viewer:

```
cargo run --release --package fitsview -- /tmp/fitsview-samples/light_orientation.fits
```

Orientation and NaN handling are also asserted automatically in
`crates/fitsview/tests/rendering.rs`, so the entries below are confirmation on
real hardware rather than the only evidence.

- [ ] Window opens at a sensible size on a fresh profile.
- [ ] `Open File…` dialog appears and filters to FITS extensions.
- [ ] Image is the right way up. `light_orientation.fits` shows its bright band
      along the **bottom** edge. Along the top means the flip was lost.
- [ ] Zoom with the scroll wheel keeps the pixel under the cursor fixed.
- [ ] Drag pans; `F` fits; `1` shows 100 %.
- [ ] Panning a 24 MP image feels smooth, with no visible stutter.
- [ ] Drag and drop a file onto the window loads it.
- [ ] `nan_test.fits` renders with scattered black pixels and visible structure,
      not a uniformly blank image.
- [ ] `colour_test.fits` shows a red-to-yellow gradient, not greyscale.
- [ ] Opening a non-FITS file, such as `notes.txt`, shows an error in the status
      bar and leaves the previous image on screen.

Status on macOS as of Phase 2: the window opens, loads a 24 MP file in about
18 ms, and downsamples it by two for display. The remaining boxes need a human
at the keyboard, and the whole list needs running on Linux and Windows.

---

## Phase 3 — Folder browsing

```
cargo run --release --package fitsview -- /tmp/fitsview-samples
```

- [ ] `Open Folder…` lists only FITS files; `notes.txt` is absent.
- [ ] The list is in natural order: `light_1`, `light_2`, `light_10`, not
      `light_1`, `light_10`, `light_2`.
- [ ] Clicking a row in the list shows that image.
- [ ] Arrow keys, Space, Page Up and Page Down move through the list, and stop
      at both ends rather than wrapping.
- [ ] Home and End jump to the first and last file.
- [ ] Holding the right arrow key through a folder of large files leaves the
      window responsive, with no beachball or freeze.
- [ ] Stepping back to a file just visited is instant, with no visible reload.
- [ ] The counter reads `n / total` and tracks the selection.
- [ ] `F5` picks up a file added to the folder from outside the application.
- [ ] Dropping a folder on the window opens it.

Automated coverage: `crates/fitsview/tests/navigation.rs` asserts that no single
step takes more than one frame at 60 fps, and that cache memory stays bounded
across a 200-file folder. The boxes above are about how it feels on real
hardware, which those tests cannot judge.

---

## Phase 4 — Delete, rename, flag

- [ ] Deleting an unflagged file shows no dialog and advances the selection.
- [ ] The deleted file is in the OS trash or recycle bin, not gone.
- [ ] Deleting a flagged file always shows the confirmation dialog.
- [ ] `Enter` does **not** confirm that dialog; `Esc` cancels it.
- [ ] `F2` renames inline, with the extension preserved.
- [ ] Renaming to an existing name is rejected with a visible message.
- [ ] Flags survive quitting and reopening the app.
- [ ] `?` shows the help overlay and every listed shortcut works.

---

## Phase 5 — Stretch

- [ ] The stretch toggle visibly brightens a linear image.
- [ ] Toggling off returns to the previous appearance.
- [ ] The setting persists across a restart.
- [ ] Stretched images look comparable to the same file in another astronomy
      viewer.

---

## Phase 6 — Dark calibration

- [ ] Adding darks of the wrong dimensions disables the toggle with a reason.
- [ ] Applying a dark visibly removes hot pixels.
- [ ] Export writes `_cal.fits` files and leaves the originals untouched.
- [ ] Cancelling an export stops it promptly and leaves no partial file.

---

## Phase 7 — Flat calibration

- [ ] A master flat built from real flats shows the expected vignetting pattern:
      bright centre, darker corners.
- [ ] Applying the flat visibly evens out corner brightness and removes dust
      shadows.
- [ ] Applying dark and flat together looks right. Dividing before subtracting
      would smear the dark signal across the frame, which shows as an uneven
      background that the stretch exaggerates.
- [ ] A deliberately bad flat, for example one taken with the lens cap on,
      produces a warning naming the number of unusable pixels rather than an
      image full of bright speckles.
- [ ] A colour flat does not turn the image grey.
- [ ] Calibration settings are restored when the folder is reopened.

---

## Phase 8 — Packaging

- [ ] The release binary runs on a machine without a Rust toolchain.
- [ ] The application icon appears in the dock, taskbar or launcher.
- [ ] A crash message appears rather than a silent exit if startup fails.
