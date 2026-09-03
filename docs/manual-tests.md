# Manual test checklist

Some acceptance criteria in `phasedbuild.md` cannot be asserted in code: whether a
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
- [ ] Left, right, up and down arrows all move through the list, along with
      Space, Page Up and Page Down, and stop at both ends rather than wrapping.
- [ ] Holding down or up scrolls the list so the highlighted row stays in view.
- [ ] `L`, and the arrow button at the left of the toolbar, collapse the file
      list so the image fills the window, and bring it back.
- [ ] Dragging the file list's right edge all the way left collapses it too, and
      the toolbar arrow flips to point the other way.
- [ ] A collapsed file list is still collapsed after quitting and reopening.
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

Work on a copy of a folder, not on frames you care about.

Run the real-trash check on this platform first, since no ordinary test
touches it:

```
cargo test -p fitsview --all-features -- --ignored real_delete
```

- [ ] Deleting an unflagged file shows no dialog and advances the selection.
- [ ] **The deleted file is in the trash or recycle bin, not gone.** Open the
      trash and confirm it is really there.
- [ ] Deleting is immediate. If it hangs for even a second, the trash back end
      is wrong for this platform; see the Phase 4 notes in `phasedbuild.md`.
- [ ] `K` flags the file, the star appears in the list and the toolbar button
      changes.
- [ ] Deleting a flagged file always shows the confirmation dialog.
- [ ] `Enter` does **not** confirm that dialog; `Esc` cancels it.
- [ ] `Shift+Delete` deletes a flagged file without the dialog.
- [ ] "Confirm every delete" makes unflagged files ask too.
- [ ] `F2` opens the rename editor with the current name, extension preserved
      when you leave it off.
- [ ] Renaming to an existing name is rejected with a visible message, and the
      editor stays open with what you typed.
- [ ] While the rename editor is open, pressing `d`, `k` or an arrow key types
      into the box and does not delete, flag or navigate.
- [ ] Flags survive quitting and reopening the application.
- [ ] Clearing the last flag removes `.fitsview.json` from the folder.
- [ ] `?` shows the help overlay, and every shortcut listed in it works.
- [ ] Deleting the last file in a folder leaves an empty window, not a crash.

---

## Phase 5 — Stretch

Use a real light frame if you have one, since synthetic samples lack the faint
structure that makes a stretch worth looking at.

- [ ] `S`, and the Stretch checkbox, visibly brighten a raw frame. A linear view
      of a real light should look nearly black beforehand.
- [ ] Toggling off returns to the previous appearance.
- [ ] Toggling is immediate on a 24 MP frame, with no visible pause.
- [ ] A colour frame keeps its colour when stretched. If it turns grey, each
      plane is being measured separately; see the Phase 5 notes in `phasedbuild.md`.
- [ ] The ⚙ menu adjusts the background level and black point, and the image
      responds as you drag.
- [ ] Reset returns both settings to their defaults.
- [ ] The stretch setting, and the settings behind ⚙, survive quitting and
      reopening the application.
- [ ] Stretched images look comparable to the same file in another astronomy
      viewer. This is the only check that the algorithm matches convention
      rather than merely being self-consistent.

---

## Phase 6 — Dark calibration

Use real darks matching a real light if you have them. Work on a copy.

- [ ] `Add darks…` accepts several files at once and the count is shown.
- [ ] `Build master` shows a progress bar and finishes without freezing the
      window. The panel then reports how many frames were combined.
- [ ] Applying the dark visibly removes hot pixels. Turn the stretch on first;
      hot pixels are easiest to see against a stretched background.
- [ ] `D` toggles it, and the image visibly changes each way.
- [ ] Adding darks of the wrong dimensions disables the toggle and shows the
      reason, and the light is still displayed uncalibrated rather than vanishing.
- [ ] A dark of a clearly different exposure shows a warning but still applies.
- [ ] `Save master…` then `Clear` then `Load master…` restores the same master,
      and the frame count survives.
- [ ] Export writes `_cal.fits` files and leaves the originals untouched. Check
      an original's modification time afterwards.
- [ ] Exporting into the source folder is safe: originals keep their names and
      contents.
- [ ] Cancelling an export stops promptly and leaves no half-written file. Open
      the last file written and confirm it reads.
- [ ] The window stays responsive during a long export: panning and stepping
      through files still work.

---

## Phase 7 — Flat calibration

Real flats matter here: synthetic ones cannot show dust shadows.

- [ ] `Add flats…` and `Build master` produce a gain map, and the panel reports
      the frame count.
- [ ] Saving the gain map and viewing it shows the expected vignetting pattern:
      bright centre, darker corners, with dust shadows as dark rings.
- [ ] `Shift+F` applies it, and corner brightness visibly evens out. Turn the
      stretch on first; vignetting is far easier to see stretched.
- [ ] Dust shadows disappear.
- [ ] Plain `F` still fits the image to the window and does not toggle the flat.
- [ ] Applying dark and flat together looks right. An uneven background that the
      stretch exaggerates would suggest the order is wrong.
- [ ] Adding flat darks changes the result, and the panel says how many will be
      subtracted.
- [ ] A flat taken with the lens cap on is refused with an explanation, rather
      than producing an image of bright speckles.
- [ ] A flat with a very dark corner reports how many pixels are unusable, and
      those pixels render black rather than as bright noise.
- [ ] A colour flat does not turn the image grey.
- [ ] A flat of the wrong dimensions disables the toggle and says so.
- [ ] Export applies both frames and the written files record it in HISTORY.
- [ ] Calibration settings are restored when the folder is reopened. Moving a
      master away and reopening skips it quietly rather than erroring.

---

## Phase 8 — Packaging

- [ ] The release binary runs on a machine without a Rust toolchain.
- [ ] The application icon appears in the dock, taskbar or launcher.
- [ ] The right panel shows the image metadata above the calibration controls,
      and both sections collapse independently.
- [ ] `I`, and clicking the section heading, both open and close the metadata,
      and agree with each other.
- [ ] The filter narrows the cards as you type, matching keywords and values,
      and the count beside it updates.
- [ ] The identifying keywords are at the top, above a rule, in this order where
      the file has them: OBJECT, TELESCOP, CAMERAID, IMAGETYP, FILTER, EXPOSURE,
      GAIN, CCD_TEMP, BAYERPAT, DATE-OBS. Everything else follows in the order
      the file wrote it.
- [ ] A file writing CCD-TEMP with a hyphen is pinned just the same as one
      writing CCD_TEMP.
- [ ] The metadata follows the selection when you step through a folder, and the
      filter stays put.
- [ ] A file with a long header scrolls within its section rather than pushing
      the calibration controls off the bottom.
- [ ] Quitting and reopening restores the folder you were in, the stretch
      setting, and the stretch parameters.
- [ ] `fitsview --help` lists every key that actually works.
- [ ] A crash shows a dialog rather than the window vanishing, and writes to the
      crash log named in that dialog. To provoke one deliberately, build with a
      temporary `panic!` early in `main`.
- [ ] Pushing a `v0.1.0` tag produces a draft release with three binaries
      attached. Check each downloads and runs.

---

## Phase 9 — One-shot colour

Needs a raw frame from a colour camera. A frame from a mono camera has no
filter grid and nothing here applies to it.

- [ ] Opening a raw frame from a colour camera shows it in colour without being
      asked, and the panel names the pattern as coming from the file.
- [ ] `B` turns reconstruction off, and the image becomes greyscale with a fine
      checkerboard visible when zoomed in. That checkerboard is the filter grid.
- [ ] The colours are right. If red and blue are swapped, or there is a magenta
      or green cast, turn on **Flip pattern rows**; that is the symptom it is
      for.
- [ ] If the flip does not fix it, try the other patterns in the chooser. One of
      the four will be correct.
- [ ] Whatever combination works is still in force after quitting and reopening
      the folder.
- [ ] A dark and a flat from the same camera still apply, and are not reported
      as the wrong size once colour is being shown.
- [ ] Calibration visibly works: hot pixels go, vignetting evens out, and the
      colours stay right rather than developing a cast.
- [ ] Exported files are still mosaics. Open one and confirm it is greyscale
      until debayered, which is what a stacker expects.
- [ ] A mono frame shows the Debayer control disabled.

---

## Phase 13 — Frame quality

Needs a real session, ideally one with a few frames spoiled by cloud or wind.

- [ ] The file list has a sort control, and choosing Background or Sharpness
      reorders it and shows the value in place of the file size.
- [ ] The selection stays on the same frame through a re-sort, and the image on
      screen does not change.
- [ ] Browsing the folder fills the values in as you go, without pressing
      Measure.
- [ ] Measure fills in the rest, with a progress bar, and the window stays
      responsive.
- [ ] Sorting by Background puts the frames taken through cloud together, and
      they do look worse.
- [ ] Sorting by Sharpness puts the blurred or trailed frames together.
- [ ] Frames unlike the rest are marked in a different colour, and nothing is
      deleted or flagged by it.
- [ ] The metadata panel shows Background, Noise and Sharpness for the current
      frame, above the header cards.
