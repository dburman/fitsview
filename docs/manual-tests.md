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
- [ ] Pushing a version tag produces a draft release with three binaries
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

---

## Phase 14 — Readouts

- [ ] Moving the pointer over the image shows coordinates and a value in the
      status bar, and it changes as you move.
- [ ] Moving off the image clears it rather than showing a stale value.
- [ ] With a dark applied, the readout still shows the file's value rather than
      the calibrated one. Toggle the dark with D and check the number does not
      move.
- [ ] On a colour frame, three values are shown, bracketed and separated by
      spaces: `[1234 2345 3456]`.
- [ ] On a one-shot colour frame with reconstruction on, three values are shown
      as `~[1234 2345 3456]`, the tilde marking them as interpolated, and they
      match the colour on screen at that point.
- [ ] Turning reconstruction off with B returns the readout to a single value,
      which is what that pixel actually measured.
- [ ] `G` shows the histogram, and it has the shape of a sky frame: one tall
      peak near the left with a thin tail.
- [ ] With the stretch on, the black point and midtone are marked on the
      histogram, and dragging the stretch settings moves them.
- [ ] The histogram updates when you step to another frame.
- [ ] It is still showing, or still hidden, after quitting and reopening.

## Phase 15 — Star detection

- [ ] The **Stars** box in the toolbar is unticked when the application first
      starts, and no circles are drawn.
- [ ] Ticking it, or pressing `Shift+S`, draws a circle on each star within a
      moment. Zoom in and check the circles sit on the stars rather than beside
      them, and that their sizes follow how big the stars look.
- [ ] Plain `S` still toggles the stretch rather than the stars.
- [ ] The toolbar and the metadata panel agree on the count, and the width and
      roundness are plausible: a few pixels wide, roundness near 1 on a frame
      that tracked.
- [ ] A deliberately defocused or trailed frame reports a larger width, or a
      lower roundness, than a good one from the same night. This is the whole
      point of the feature; if it does not hold, the numbers are not usable.
- [ ] Holding the arrow key down through a folder with stars on is still
      smooth, and no frame ever shows the previous frame's circles.
- [ ] On a one-shot colour frame, each star is circled once rather than as a
      cluster of four, with reconstruction both on and off.
- [ ] The gear beside the box is greyed out until detection is on. Opening it
      and lowering the threshold finds more stars; **Reset** returns to the
      original count.
- [ ] Step through the folder with the arrow keys and do not touch the mouse.
      The circles and the figures follow each frame on their own; they never
      wait for the pointer to move before catching up.
- [ ] Untick the box: the circles go, and the figures leave the metadata panel.
- [ ] Tick it, quit, and reopen: it is still ticked, and detection starts on
      the frame that reopens.

## Read-only volumes

Needs a drive macOS cannot write to. An NTFS-formatted external disk is the
easiest: macOS mounts it read-only. `mount | grep -i <name>` should show
`read-only`.

- [ ] Opening a folder on it shows the images normally, and a toast says the
      volume is read-only.
- [ ] The status line keeps saying *Read-only volume* after the toast has gone,
      and after stepping to another frame.
- [ ] Rename and Delete in the toolbar are greyed out, and hovering either says
      why.
- [ ] `Delete` on the keyboard gives the read-only message rather than a
      confirmation dialog, and the file is still there afterwards.
- [ ] `F2` gives the same message rather than opening the rename editor.
- [ ] `K` still marks the frame to keep for this session, and says the mark
      will not be saved.
- [ ] Nothing named `.fitsview*` is left on the drive afterwards.
- [ ] Open a folder on the internal disk again: deleting, renaming and flagging
      all work as before, and the status line no longer mentions read-only.

## Message text

- [ ] Narrow the window as far as it goes, then press `Shift+S`. The toast
      reads *Finding stars…* on one line, or wrapped between words — never
      broken in the middle of one.
- [ ] The same with the read-only message, which is the longest the
      application produces: it wraps between words and stays inside the window.
- [ ] With stars on and the window narrow, the summary beside the toolbar
      checkbox is not broken mid-word either.

## Folders and zoom

- [ ] Open a target folder holding `date/frames` or `date/filter/frames`. Every
      night's frames are listed, grouped by folder, with the folder shown as
      part of the name.
- [ ] A folder three levels down is not listed. Two is the limit.
- [ ] Two nights that each hold `light_0001.fits` both appear, and marking one
      to keep does not mark the other.
- [ ] Rename a frame that sits in a subfolder: it keeps its place in that
      subfolder rather than moving to the top, and the list still shows it
      under its night.
- [ ] Zoom in, then step to the next frame with the arrow keys. The zoom and
      the position stay where they were.
- [ ] Step to a frame from a different camera, of a different size. That one is
      fitted to the window instead, since the old view means nothing for it.

## Neutral background

- [ ] On a debayered one-shot colour frame with the stretch on, the sky is
      green. That is right: it is what the sensor recorded.
- [ ] Ticking **Neutral background** in the one-shot colour section turns the
      sky grey, and the stars keep their colours.
- [ ] Untick it and the green comes back.
- [ ] On a mono frame the setting changes nothing at all.
- [ ] With it on, the histogram no longer marks the black point and midtone,
      because the three channels no longer share one.
- [ ] It is still set the way you left it after quitting and reopening.

## The right pane, and the file list

- [ ] The FITS keywords that identify a frame — OBJECT, TELESCOP, FILTER,
      EXPOSURE and the rest — are at the top of the right pane, above the
      measurements.
- [ ] **Other header cards** below them is shut when the application starts,
      and its count matches what is inside once opened.
- [ ] Opening it and typing in the filter searches those cards only; the ones
      pinned above stay where they are.
- [ ] On a mono frame the **One-shot colour** section is not shown at all.
- [ ] On a raw colour frame it is, whether or not the file declares BAYERPAT.
- [ ] Once a frame has been debayered the section is still shown, so colour can
      be turned off again.
- [ ] Clicking a file's **name** in the list selects it, not only the row
      around the name.

## Star width

- [ ] With stars on, the width is shown in pixels and in arcseconds, and
      FOCALLEN and XPIXSZ are among the keywords pinned at the top of the pane.
- [ ] The arcsecond figure is believable for the sky you shot under: single
      figures at worst, not tens.
- [ ] A frame taken with the cover on reports neither a width nor a roundness,
      since what it finds are noise blobs rather than stars.
- [ ] A frame from a camera whose header omits the focal length shows the width
      in pixels alone, with no arcseconds and no invented scale.

## Measuring a folder for stars

- [ ] The ordering menu offers Width and Roundness after Background and
      Sharpness.
- [ ] **Measure stars** sits beside **Measure** and shows how far it has got.
- [ ] Running it fills width and roundness for every frame, and the background
      and sharpness with them.
- [ ] Ordering by Width puts the sharpest frames at one end; the column shows
      arcseconds where the header gives the optics, pixels where it does not.
- [ ] Ordering by Roundness puts the trailed frames at one end.
- [ ] A frame far from the rest of the folder is coloured as unusual, which is
      the frame worth opening.
- [ ] Plain **Measure** does not fill the star columns, and is still quick on a
      folder of full-frame captures.
- [ ] Stopping the job partway keeps the frames it had already measured.

## Measurements remembered

- [ ] Measure a folder for stars, close the application, and open the folder
      again: every column is filled at once, and neither **Measure** button is
      shown.
- [ ] Open one night, or one filter, of a target that was measured by opening
      the target: its figures are there too.
- [ ] Measure a few frames by viewing them, then press **Measure**: only the
      frames not yet measured are read, and the count says so.
- [ ] Change the star detection settings: **Measure stars** comes back, and
      running it finds the stars again under the new settings.
- [ ] Rename a measured frame, reopen the folder: it still has its figures.
- [ ] Recalibrate or otherwise rewrite a frame in place, reopen the folder: that
      frame is blank and the rest are not.
- [ ] Each folder that holds frames gains a hidden `.fitsview-measurements.json`
      after measuring, and nothing visible.
- [ ] On a read-only volume measuring still works, nothing is written, and no
      error is shown.

## During a session

Easiest with the capture software running; copying frames into an open folder
one at a time does nearly as well.

- [ ] A frame saved into the open folder joins the list within a few seconds,
      without touching the mouse or keyboard.
- [ ] With the folder measured, the new frame's columns fill on their own; with
      star figures in the folder, its width and roundness fill too.
- [ ] A frame with a much brighter sky than the rest is named in a message
      saying what stands out. An ordinary frame arrives without one.
- [ ] Sitting on the last frame, in name order, moves to each new frame as it
      arrives. Stepping back to an earlier frame stops that.
- [ ] A frame deleted or renamed in Finder or Explorer leaves the list.
- [ ] Stacking writes its stacks into the list without jumping to them.
- [ ] A frame opened while it is still being written says so in the status
      line, not as an error, and opens once it is complete.
- [ ] A file cut short long ago is still reported as damaged.
- [ ] On a network share the list still keeps up, if more slowly.

## Rejecting outliers

- [ ] Stack a night whose sky brightens: the samples rejected, shown when the
      stack finishes, are well under one per cent, not several.
- [ ] A frame crossed by a satellite stacks without the trail.
- [ ] A night that crosses the meridian stacks with rejection on without a
      ghost of the trail or a soft patch where the frames were turned.
- [ ] With rejection on, a stack of five frames still leaves out a satellite in
      one of them; with four it says nothing was rejected.
- [ ] Stacking a long night of full frames with rejection does not run the
      machine out of memory.

## Placing frames between pixels

- [ ] Stack a night and compare its stars with a 0.1.8 stack of the same
      frames: they should be slightly narrower and rounder, never doubled.
- [ ] The sky of the new stack looks a little grainier at full size than the
      0.1.8 one did. That is the grain the frames have; blending was smoothing
      it away along with the stars.
- [ ] Zoomed in on the brightest stars, there is no dark ring around them.
- [ ] A night that crosses the meridian stacks with its stars single and round
      right to the corners.

## Stacking when memory is short

- [ ] On a 16 GB machine, stack a night of full-frame colour frames with
      rejection: the message at the end says it went a colour at a time, and
      the machine does not grind to a halt swapping.
- [ ] On a 32 GB or larger machine the same stack says nothing of colours.
- [ ] A mono night is never split, whatever the machine.
- [ ] The two stacks of the same night, split and not, look the same.

## The calibration library

- [ ] Choose a library folder in the Calibration panel: it says how many dark,
      flat and bias sets it found, and remembers the folder after a restart.
- [ ] With a night open, each filter shows the dark and flat it will get, or in
      warning colour why there is none.
- [ ] A flat more than a month from the night says how many days apart.
- [ ] Stack: the first time takes longer while masters are made; the progress
      line says which. A hidden `.fitsview-masters` folder appears in the
      library. The second stack is as quick as without a library.
- [ ] The stack's header history names the dark and flat used.
- [ ] Load a master dark by hand: the panel and the stack both say the one
      chosen by hand was used.
- [ ] With the library on an external drive that is unplugged, the panel says
      it is not found, and stacking goes ahead without it.
- [ ] Add frames to the library and press **Read again**: they are found.
