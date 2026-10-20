# Private SDR nightlight CTM backend

This patch adds **`drm-ctm-v1`**, not gamma-LUT emulation. It targets the Apple
Asahi display controller, which exposes CRTC CTM but zero gamma/degamma entries.
No render/thumbnail/shader changes, privileged helper, new DRM open/master, or
audio changes are involved. A narrowly patched private Smithay dependency
provides cooperative pre-disable cleanup and retained saved-state CTM blobs.

## Comparison and hardware path

Inspected upstream tags matching installed versions:

- Hyprsunset v0.4.0 (`25f704346ec22e7623b0873ef8c4573b57ca1512`),
  `src/Hyprsunset.cpp`: `matrixForKelvin`, `applyCTM`, `commitCTMs`. Its
  temperature control is a diagonal color matrix, not a gamma LUT.
- Hyprland v0.56.1 (`5c9377c15f85c50648f35ca5a213754f95b93ca0`),
  `src/protocols/CTMControl.cpp`, `src/output/Monitor.cpp`,
  `src/render/Renderer.cpp`: CTM protocol -> monitor CTM -> output state.
- Aquamarine v0.15.0 (`783bfd9ae441d1d0519b979ac68b73ddd6e81df0`),
  `src/backend/drm/impl/Atomic.cpp`: converts CTM to DRM sign-magnitude S31.32
  and submits the CRTC property, with blob commit/rollback ownership.
- AsahiLinux/linux `asahi-wip`, `drivers/gpu/drm/apple/apple_drv.c`: calls
  `drm_crtc_enable_color_mgmt(..., 0, true, 0)`. `iomfb_template.c` sends the
  matrix to the display coprocessor (`iomfb_set_matrix`, location 9) when
  `color_mgmt_changed`; zero CTM uses identity. This is a final output transform,
  not a per-window effect. These kernel sources are reference material, not a
  claim of exact identity with the installed downstream kernel or tested writes.

The new code uses the compositor's existing DRM FD. A blocking CTM property
commit is followed by matrix readback before IPC success. All KMS planes pass
through the CRTC CTM, including fullscreen direct scanout, cursors and overlays.
Scanout optimizations therefore do not bypass this effect, unlike a shader
attached only to selected windows. Screencopy reads pre-CTM buffers; screenshots
are not a measurement of physical panel warmth.

## Calibration and ownership

At output creation capture the original CTM **contents**, including signed
coefficients and a zero-blob distinction. For column RGB vectors apply
`calibration * temperature * RGB` (scale calibration's columns). 4000 K uses
the same Tanner Helland approximation used by Hyprsunset; off restores the
captured bytes exactly rather than replacing calibration with identity.

Readback detects an external calibration change and refuses to overwrite it.
Only this object's newly allocated blobs are destroyed. Restore may create an
identical-content blob with a new ID because the prior owner's blob can cease
to exist after replacement. Original calibration values, not an ephemeral ID,
are the invariant.

Smithay also retains its OWN copied CTM blob for the entire atomic saved-state
lifetime and substitutes that valid ID in its shutdown restoration request.
Otherwise its inherited ID could disappear and invalidate the whole console
restore. Dependency baseline is `ff5fa7df392cecfba049ffed55cdaa4e98a8e7ef`,
private revision `cf416bfa131a9f27eabc9a0bc00b7472b73b6fad`, in
`/home/ali/code/niri-nightlight-smithay`. No dependency version upgrades.

An IPC connection owns the warm-state lease. Other connections can query but
cannot change an active lease. Disable, controller disconnect, output release,
cooperative seat pause and graceful compositor shutdown attempt checked
restoration of the captured matrix. Resume
and output hotplug reconcile the retained connection intent. Unsupported newly
attached outputs cause restoration rather than falsely claiming all-output
success. A partial write failure rolls previously changed outputs back; rollback
failures remain errors and readback cannot falsely report applied state.
Cleanup ownership is reserved BEFORE writes. Uncertain writes retain owned blob
IDs and dirty state independently of acknowledged enable. A three-second timer
retries safe restoration when leased desired state is false, EVEN if the failed
enable's connection remains open. Successful warm leases are not undone.
Disable cleanup never re-warms already restored outputs to disguise a partial
failure. Retired surfaces retain cleanup until recovery or explicit lost-device
diagnostics. External calibration blobs are never overwritten by cleanup.

## Explicit external-force / virtual-terminal limitation

**Turn nightlight OFF and confirm status before switching virtual terminals.**
There is NO guarantee of neutral CTM on an external forced VT switch. This is
an accepted limitation of this implementation, not a review-approved guarantee.

Inspected installed-version systemd 261.3 sources (`3255daee1572366b74fe92f002a3d60ecbb27103`):
`session_leave_vt()` calls `session_device_pause_all()`; that stops DRM devices
(drops master) BEFORE sending PauseDevice `force`. libseat 0.9.3 ignores `force`
there and emits its disable callback on a later Active change. Our synchronous
Smithay pre-disable hook precedes cooperative acknowledgements (including
logind's early PauseDeviceComplete), but cannot reverse prior revocation.
Warmth can remain on the next VT, and another compositor can inherit a warm CTM
as its initial calibration until the property is reset. Returning to this Niri
session permits checked cleanup/reapplication, subject to external conflicts.

Inspected Hyprland v0.56.1/Aquamarine v0.15.0 use the same libseat/logind path:
Aquamarine's disable listener suspends input/emits activity/acknowledges;
DRM deactivation cancels pending frames but does not restore neutral CTM.
Hyprland's VT key path directly calls switchVT. Neither inspected path provides
a stronger pre-revocation guarantee. This is SOURCE evidence, not a live VT test.

Orderly disable/logout while DRM is owned has checked calibration restoration
and a valid saved-state blob fallback; forced termination, session-manager
revocation before cleanup, or hardware failure cannot guarantee it. No logind,
VT signal-ownership machinery, kernel, privileges or seat-backend changes are
made. A final-output shader would avoid persistent KMS tint but requires a
separate rendering/scanout project and is deliberately outside this scope.

Abrupt compositor SIGKILL/kernel/device failure cannot execute cleanup; no
userspace implementation can guarantee physical restoration after that. This
patch does not alter security/kernel policy or blindly replace a new external
calibration on the next login. Normal lifecycle and failure paths need physical
testing after the new native compositor is activated.

## IPC

On a persistent connection to `$NIRI_SOCKET`, send newline-delimited JSON:

```
{"Nightlight":{"enabled":null}}
{"Nightlight":{"enabled":true}}
{"Nightlight":{"enabled":false}}
```

Each successful reply is `{"Ok":{"Nightlight":{...}}}`; failed writes are
`{"Err":"..."}`. Status includes backend version, per-output capability,
calibration words, verified enabled state, session activity and errors.
`enabled` is `true` only when all outputs verify warm, `false` only when ALL
verify restored, and `null` for mixed/inactive/unknown state. `cleanup_pending`
records restoration owed even without an acknowledged lease. `generation`
changes on output reconciliation/resume, not polling. The controller retries
persisted enable once per meaningful transition; gamma fallback retains a
read-only native capability probe so CTM hotplug can promote it without
repeated gamma acquisition. EventStream is rejected on a connection that has
used nightlight writes; use a separate event connection to preserve the lease.
`supported` means the property and readable calibration exist; only a successful
write/readback establishes application. A nested/headless backend reports no
native DRM support and rejects writes; it never calls `tty()` and panics or
borrows its parent compositor's output.

Settings persistence belongs to the shared user-level Niri controller. It keeps
one lease, writes intent only after acknowledgment and re-applies on next login.
The packaged `/usr/bin/niri msg` need not understand a new action because the
controller speaks the JSON request directly. Existing gamma protocol stays
unchanged for clients/displays that actually support LUTs.

## Validation

`nice -n 10 cargo test --release --locked -j 2 --lib -- --test-threads=2`
includes sign/magnitude and non-diagonal calibration composition, exact neutral
restore, repeated toggles/drop, zero blob, rejected writes, bad readback,
disable failure, external calibration conflicts and multi-output rollback.
User-profile tests cover connection ownership, persistence, restart and truthful
old-compositor messaging. Native DRM write/readback and physical warmth still
require login into the newly built compositor; a nested run cannot prove them.

Keep this patch independent of the tuned thumbnail commits. For base, apply it
to the installed upstream revision and use a separate private executable. For
tuned, build on top of the complete current tuned history. Never overwrite the
packaged executable or restart an active desktop to activate a build silently.
