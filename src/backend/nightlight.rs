//! Output-wide SDR nightlight using the compositor's existing DRM CTM.
//!
//! Not a gamma-LUT emulation. Hardware CRTC CTM covers all planes, including
//! direct scanout, layers and cursors; no renderer or thumbnail changes needed.
//! Composition is C * N for column RGB vectors: apply temperature in source RGB,
//! then the captured calibration C. Disable restores C byte-for-byte, not identity.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use anyhow::{bail, ensure, Context};
use smithay::backend::drm::{DrmDevice, DrmDeviceFd};
use smithay::reexports::drm::control::{crtc, property, Device};

pub type Matrix = [u64; 9];
const ONE: u64 = 1 << 32;
const SIGN: u64 = 1 << 63;
pub const IDENTITY: Matrix = [ONE, 0, 0, 0, ONE, 0, 0, 0, ONE];

/// Same 4000 K approximation as Hyprsunset 0.4.0 (Tanner Helland algorithm).
pub fn warm_gains() -> [f64; 3] {
    let t = 4000.0_f64 / 100.0;
    [
        1.0,
        (99.4708025861 * t.ln() - 161.1195681661).clamp(0.0, 255.0) / 255.0,
        (138.5177312231 * (t - 10.0).ln() - 305.0447927307).clamp(0.0, 255.0) / 255.0,
    ]
}

fn warm_matrix(calibration: Matrix) -> anyhow::Result<Matrix> {
    let gains = warm_gains();
    let mut result = calibration;
    for (i, word) in calibration.into_iter().enumerate() {
        // DRM CTM is SIGN-MAGNITUDE S31.32, not two's complement.
        let magnitude = (word & !SIGN) as f64 * gains[i % 3];
        ensure!(
            magnitude.is_finite() && magnitude < SIGN as f64,
            "CTM coefficient out of range"
        );
        result[i] = (word & SIGN) | magnitude as u64;
    }
    Ok(result)
}

pub trait CtmIo {
    fn read(&self) -> anyhow::Result<Option<Matrix>>;
    /// Blocking property commit. Never opens another DRM node or claims master.
    fn write(&mut self, matrix: Option<Matrix>) -> anyhow::Result<()>;
    /// Whether the CURRENT property references a blob allocated and retained
    /// by this object. Needed to recover a write whose readback was uncertain
    /// without overwriting an external calibration blob.
    fn owns_current(&self) -> anyhow::Result<bool>;
}

pub struct Ctm<I: CtmIo> {
    io: I,
    baseline: Option<Matrix>,
    expected: Option<Matrix>,
    enabled: bool,
    dirty: bool,
    error: String,
}

impl<I: CtmIo> Ctm<I> {
    pub fn new(io: I) -> anyhow::Result<Self> {
        // Copy the contents, not merely the blob ID: the previous owner can
        // release its blob after our first write. No property write at discovery.
        let baseline = io.read()?;
        Ok(Self {
            io,
            baseline,
            expected: baseline,
            enabled: false,
            dirty: false,
            error: String::new(),
        })
    }

    pub fn baseline(&self) -> Option<Matrix> {
        self.baseline
    }

    pub fn verified(&self) -> anyhow::Result<bool> {
        ensure!(
            !self.dirty,
            "CTM state uncertain; cleanup pending: {}",
            self.error
        );
        ensure!(
            self.io.read()? == self.expected,
            "CTM changed outside nightlight; refusing to overwrite calibration"
        );
        Ok(self.enabled)
    }

    pub fn set(&mut self, enabled: bool) -> anyhow::Result<()> {
        if !enabled {
            return self.restore();
        }
        self.verified()?;
        if enabled == self.enabled {
            return Ok(());
        }
        let next = if enabled {
            Some(warm_matrix(self.baseline.unwrap_or(IDENTITY))?)
        } else {
            self.baseline
        };
        let previous = self.expected;
        // Ownership/cleanup is recorded BEFORE the first potentially successful
        // write. A failed enable is not proof that the display stayed neutral.
        self.dirty = true;
        let applied = self.io.write(next).and_then(|()| {
            ensure!(
                self.io.read()? == next,
                "CTM readback did not match committed matrix"
            );
            Ok(())
        });
        if let Err(error) = applied {
            // Restore the exact previous calibration/temperature, even if a
            // driver failed after submission. Never claim success after rollback.
            let rollback = self.io.write(previous).and_then(|()| {
                ensure!(
                    self.io.read()? == previous,
                    "CTM rollback readback mismatch"
                );
                Ok(())
            });
            self.dirty = rollback.is_err();
            self.error = format!("CTM commit failed: {error:#}; rollback: {rollback:?}");
            bail!("{}", self.error);
        }
        self.expected = next;
        self.enabled = enabled;
        self.dirty = false;
        self.error.clear();
        Ok(())
    }

    pub fn cleanup_pending(&self) -> bool {
        self.enabled || self.dirty
    }

    /// Cleanup is independent of confirmed enable state and of the IPC lease.
    /// It is deliberately NOT a transaction: never re-warm a restored output
    /// merely because another output could not be restored.
    pub fn restore(&mut self) -> anyhow::Result<()> {
        let result = self.restore_inner();
        if let Err(err) = &result {
            self.error = format!("{err:#}");
        }
        result
    }

    fn restore_inner(&mut self) -> anyhow::Result<()> {
        let current = self.io.read()?;
        if current == self.baseline {
            self.expected = self.baseline;
            self.enabled = false;
            self.dirty = false;
            self.error.clear();
            return Ok(());
        }
        ensure!(
            self.cleanup_pending() && self.io.owns_current()?,
            "CTM changed externally; refusing to overwrite calibration (cleanup pending)"
        );
        self.dirty = true;
        self.io.write(self.baseline)?;
        ensure!(
            self.io.read()? == self.baseline,
            "CTM restoration readback failed; cleanup pending"
        );
        self.expected = self.baseline;
        self.enabled = false;
        self.dirty = false;
        self.error.clear();
        Ok(())
    }

    pub fn output_status(&self, name: String, active: bool) -> niri_ipc::NightlightOutput {
        let result = if active {
            self.verified()
        } else {
            Err(anyhow::anyhow!("DRM session inactive; state unverified"))
        };
        let (enabled, error) = match result {
            Ok(enabled) => (Some(enabled), self.error.clone()),
            Err(err) => (None, format!("{err:#}")),
        };
        niri_ipc::NightlightOutput {
            name,
            supported: true,
            enabled,
            cleanup_pending: self.cleanup_pending(),
            calibration: self.baseline,
            error,
        }
    }
}

impl<I: CtmIo> Drop for Ctm<I> {
    fn drop(&mut self) {
        if self.cleanup_pending() {
            if let Err(err) = self.restore() {
                warn!("could not restore nightlight calibration on output release: {err:#}");
            }
        }
    }
}

/// All-output policy shared with native integration; rollback on any failure.
pub fn transaction<I: CtmIo>(outputs: &mut [&mut Ctm<I>], enabled: bool) -> anyhow::Result<()> {
    if !enabled {
        let errors: Vec<_> = outputs
            .iter_mut()
            .filter_map(|o| o.restore().err())
            .collect();
        ensure!(errors.is_empty(), "CTM restoration incomplete: {errors:?}");
        return Ok(());
    }
    let previous = outputs
        .iter()
        .map(|o| o.verified())
        .collect::<anyhow::Result<Vec<_>>>()?;
    for output in outputs.iter_mut() {
        if let Err(err) = output.set(enabled) {
            let mut rollback_errors = Vec::new();
            for (output, previous) in outputs.iter_mut().zip(previous) {
                if let Err(err) = output.set(previous) {
                    rollback_errors.push(format!("{err:#}"));
                }
            }
            bail!("{err:#}; output rollback errors: {rollback_errors:?}");
        }
    }
    Ok(())
}

pub struct DrmCtmIo {
    fd: DrmDeviceFd,
    crtc: crtc::Handle,
    prop: property::Handle,
    retained_blobs: Vec<u64>,
    access_allowed: Rc<Cell<bool>>,
}

impl DrmCtmIo {
    pub fn discover(
        device: &DrmDevice,
        crtc: crtc::Handle,
        access_allowed: Rc<Cell<bool>>,
    ) -> anyhow::Result<Ctm<Self>> {
        ensure!(
            access_allowed.get(),
            "DRM session inactive; defer CTM discovery"
        );
        for (prop, _) in device.get_properties(crtc)? {
            let info = device.get_property(prop)?;
            if info.name().to_bytes() != b"CTM" {
                continue;
            }
            ensure!(
                matches!(info.value_type(), property::ValueType::Blob),
                "CTM is not a blob property"
            );
            ensure!(info.mutable(), "CTM is immutable");
            return Ctm::new(Self {
                fd: device.device_fd().clone(),
                crtc,
                prop,
                retained_blobs: Vec::new(),
                access_allowed,
            });
        }
        bail!("output has no DRM CTM property")
    }
}

impl CtmIo for DrmCtmIo {
    fn read(&self) -> anyhow::Result<Option<Matrix>> {
        ensure!(
            self.access_allowed.get(),
            "DRM session inactive; no CTM I/O permitted"
        );
        let props = self.fd.get_properties(self.crtc)?;
        let value = props
            .into_iter()
            .find(|(p, _)| *p == self.prop)
            .context("CTM property disappeared")?
            .1;
        if value == 0 {
            return Ok(None);
        }
        let data = self
            .fd
            .get_property_blob(value)
            .context("cannot read existing CTM calibration")?;
        ensure!(data.len() == 72, "invalid CTM calibration blob size");
        let mut matrix = [0; 9];
        for (word, bytes) in matrix.iter_mut().zip(data.chunks_exact(8)) {
            *word = u64::from_ne_bytes(bytes.try_into().unwrap());
        }
        Ok(Some(matrix))
    }

    fn write(&mut self, matrix: Option<Matrix>) -> anyhow::Result<()> {
        ensure!(
            self.access_allowed.get(),
            "DRM session inactive; no CTM I/O permitted"
        );
        // Bound retained failure blobs whenever the property can be read. Keep
        // the current ID as ownership evidence until the next verified write.
        if let Ok(props) = self.fd.get_properties(self.crtc) {
            if let Some((_, current)) = props.into_iter().find(|(p, _)| *p == self.prop) {
                self.retained_blobs.retain(|id| {
                    if *id == current {
                        true
                    } else {
                        let _ = self.fd.destroy_property_blob(*id);
                        false
                    }
                });
            }
        }
        let blob = if let Some(matrix) = matrix {
            self.fd
                .create_property_blob(&matrix)?
                .as_blob()
                .context("invalid CTM blob ID")?
        } else {
            0
        };
        // Keep even an unsuccessfully submitted blob alive: the driver may have
        // applied it before returning an error. Its ID is our recovery proof.
        if blob != 0 {
            self.retained_blobs.push(blob);
        }
        if let Err(err) =
            self.fd
                .set_property(self.crtc, self.prop, property::Value::Blob(blob).into())
        {
            return Err(err).context("DRM rejected CTM commit");
        }
        Ok(())
    }

    fn owns_current(&self) -> anyhow::Result<bool> {
        ensure!(
            self.access_allowed.get(),
            "DRM session inactive; no CTM I/O permitted"
        );
        let value = self
            .fd
            .get_properties(self.crtc)?
            .into_iter()
            .find(|(p, _)| *p == self.prop)
            .context("CTM property disappeared")?
            .1;
        Ok(value != 0 && self.retained_blobs.contains(&value))
    }
}

impl Drop for DrmCtmIo {
    fn drop(&mut self) {
        // Only blobs allocated by this object. A currently referenced blob is
        // kept alive by KMS itself; never destroy another owner's original ID.
        // After seat revocation do not touch the device; fd close releases our
        // creator references. No property writes occur in this destructor.
        if self.access_allowed.get() {
            for blob in &self.retained_blobs {
                let _ = self.fd.destroy_property_blob(*blob);
            }
        }
    }
}

/// Kept independently of surfaces and leases so a failed/paused disconnect
/// cannot discard uncertain cleanup state or recapture a warm CTM as baseline.
pub struct Registry<I: CtmIo = DrmCtmIo> {
    pub access: Rc<Cell<bool>>,
    entries: RefCell<Vec<(u64, crtc::Handle, String, Rc<RefCell<Ctm<I>>>)>>,
    lost: RefCell<Vec<niri_ipc::NightlightOutput>>,
}
impl<I: CtmIo> Registry<I> {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            access: Rc::new(Cell::new(false)),
            entries: RefCell::new(Vec::new()),
            lost: RefCell::new(Vec::new()),
        })
    }
}
impl Registry<DrmCtmIo> {
    pub fn track(
        &self,
        node: u64,
        device: &DrmDevice,
        crtc: crtc::Handle,
        name: String,
    ) -> anyhow::Result<Rc<RefCell<Ctm<DrmCtmIo>>>> {
        ensure!(!self.lost.borrow().iter().any(|o| o.name == name), "previous device disappeared with unrestored CTM; refusing to capture a possibly warm baseline");
        let mut entries = self.entries.borrow_mut();
        if let Some((_, _, _, ctm)) = entries.iter().find(|(n, c, _, _)| *n == node && *c == crtc) {
            return Ok(ctm.clone());
        }
        let ctm = Rc::new(RefCell::new(DrmCtmIo::discover(
            device,
            crtc,
            self.access.clone(),
        )?));
        entries.push((node, crtc, name, ctm.clone()));
        Ok(ctm)
    }
}
impl<I: CtmIo> Registry<I> {
    /// A failed initial enable has a reserved owner but NO acknowledged warm
    /// intent. Retry its safe restoration without waiting for socket loss or a
    /// topology transition. Never undo an acknowledged, desired warm lease.
    pub fn retry_unacknowledged(&self, active: bool, desired: bool) {
        if active && !desired {
            self.restore(false);
        }
    }

    pub fn restore(&self, orphans_only: bool) {
        if !self.access.get() {
            return;
        }
        for (_, _, name, ctm) in self.entries.borrow().iter() {
            if orphans_only && Rc::strong_count(ctm) != 1 {
                continue;
            }
            let mut ctm = ctm.borrow_mut();
            if ctm.cleanup_pending() {
                if let Err(err) = ctm.restore() {
                    warn!("{name}: CTM cleanup remains pending: {err:#}");
                }
            }
        }
        self.entries
            .borrow_mut()
            .retain(|(_, _, _, ctm)| Rc::strong_count(ctm) > 1 || ctm.borrow().cleanup_pending());
    }
    /// Called INSIDE libseat's disable callback, not the queued PauseSession.
    pub fn pre_disable(&self) {
        self.restore(false);
        self.access.set(false); // All later Ctm I/O refuses until ActivateSession.
    }
    pub fn forget_device(&self, node: u64) {
        // The device is leaving; retain diagnostics, but release fd references
        // before Smithay/libseat closes it. Do not pretend an impossible restore
        // succeeded, or allow recapturing this connector's uncertain baseline.
        self.entries.borrow_mut().retain(|(n, _, name, ctm)| {
            if *n != node {
                return true;
            }
            let mut ctm = ctm.borrow_mut();
            if ctm.cleanup_pending() {
                if self.access.get() {
                    let _ = ctm.restore();
                }
                if ctm.cleanup_pending() {
                    let mut status = ctm.output_status(name.clone(), false);
                    status.supported = false;
                    status.error =
                        "Device removed before CTM restoration could be confirmed".into();
                    self.lost.borrow_mut().push(status);
                }
            }
            false
        });
    }
    pub fn orphan_status(&self) -> Vec<niri_ipc::NightlightOutput> {
        let mut outputs = self.lost.borrow().clone();
        outputs.extend(
            self.entries
                .borrow()
                .iter()
                .filter(|(_, _, _, ctm)| Rc::strong_count(ctm) == 1)
                .map(|(_, _, name, ctm)| {
                    ctm.borrow()
                        .output_status(format!("{name} (disconnected)"), self.access.get())
                }),
        );
        outputs
    }
}

pub fn aggregate(
    mut outputs: Vec<niri_ipc::NightlightOutput>,
    desired: bool,
    active: bool,
    generation: u64,
) -> niri_ipc::NightlightStatus {
    outputs.sort_by(|a, b| a.name.cmp(&b.name));
    let supported = !outputs.is_empty() && outputs.iter().all(|o| o.supported);
    let enabled =
        if active && !outputs.is_empty() && outputs.iter().all(|o| o.enabled == Some(false)) {
            Some(false)
        } else if active && supported && outputs.iter().all(|o| o.enabled == Some(true)) {
            Some(true)
        } else {
            None
        };
    let cleanup_pending = outputs.iter().any(|o| o.cleanup_pending);
    let errors: Vec<_> = outputs
        .iter()
        .filter(|o| !o.error.is_empty())
        .map(|o| format!("{}: {}", o.name, o.error))
        .collect();
    let error = if !active {
        "DRM session inactive; state unverified".into()
    } else if !errors.is_empty() {
        errors.join("; ")
    } else if !supported {
        "One or more outputs lack CTM control".into()
    } else if enabled != Some(desired) {
        "CTM state is partial/unverified or requested restoration is incomplete".into()
    } else {
        String::new()
    };
    niri_ipc::NightlightStatus {
        backend: "drm-ctm-v1".into(),
        supported,
        enabled,
        desired,
        session_active: active,
        generation,
        cleanup_pending,
        temperature: (enabled == Some(true)).then_some(4000),
        outputs,
        error,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use super::*;

    #[derive(Clone, Default)]
    struct Fake {
        value: Rc<Cell<Option<Matrix>>>,
        writes: Rc<RefCell<Vec<Option<Matrix>>>>,
        reject: Rc<Cell<usize>>,
        corrupt: Rc<Cell<bool>>,
        owned: Rc<Cell<bool>>,
        reads: Rc<Cell<usize>>,
        reject_at: Rc<RefCell<Vec<usize>>>,
        fail_read_at: Rc<RefCell<Vec<usize>>>,
        access: Option<Rc<Cell<bool>>>,
    }
    impl CtmIo for Fake {
        fn read(&self) -> anyhow::Result<Option<Matrix>> {
            ensure!(self.access.as_ref().is_none_or(|a| a.get()), "revoked");
            let n = self.reads.get() + 1;
            self.reads.set(n);
            ensure!(
                !self.fail_read_at.borrow().contains(&n),
                "injected readback failure"
            );
            Ok(self.value.get())
        }
        fn write(&mut self, value: Option<Matrix>) -> anyhow::Result<()> {
            ensure!(self.access.as_ref().is_none_or(|a| a.get()), "revoked");
            self.writes.borrow_mut().push(value);
            ensure!(
                !self
                    .reject_at
                    .borrow()
                    .contains(&self.writes.borrow().len()),
                "injected indexed write failure"
            );
            if self.reject.get() > 0 {
                self.reject.set(self.reject.get() - 1);
                bail!("injected driver rejection");
            }
            self.value.set(if self.corrupt.replace(false) {
                Some([9; 9])
            } else {
                value
            });
            self.owned.set(true);
            Ok(())
        }
        fn owns_current(&self) -> anyhow::Result<bool> {
            Ok(self.owned.get())
        }
    }
    fn calibrated() -> Matrix {
        [
            ONE,
            ONE / 4,
            SIGN | ONE / 8,
            SIGN | ONE / 16,
            ONE * 2,
            ONE / 8,
            ONE / 16,
            SIGN,
            ONE,
        ]
    }
    #[test]
    fn nightlight_matches_hyprsunset_temperature_math() {
        let g = warm_gains();
        assert_eq!(g[0], 1.0);
        assert!((g[1] - 0.80712247756).abs() < 1e-9);
        assert!((g[2] - 0.65129941968).abs() < 1e-9);
    }
    #[test]
    fn nightlight_composes_columns_preserves_signed_calibration() {
        let base = calibrated();
        let warm = warm_matrix(base).unwrap();
        for i in 0..9 {
            assert_eq!(warm[i] & SIGN, base[i] & SIGN);
            let expected = ((base[i] & !SIGN) as f64 * warm_gains()[i % 3]) as u64;
            assert_eq!(warm[i] & !SIGN, expected);
        }
        assert_ne!(warm[1], base[1]); // C*N, not N*C (which leaves row 0 unchanged).
        assert_eq!(warm[3], base[3]);
    }
    #[test]
    fn nightlight_repeat_and_drop_restore_exact_bytes() {
        let io = Fake::default();
        io.value.set(Some(calibrated()));
        {
            let mut ctm = Ctm::new(io.clone()).unwrap();
            assert!(io.writes.borrow().is_empty());
            for _ in 0..20 {
                ctm.set(true).unwrap();
                assert!(ctm.verified().unwrap());
                ctm.set(false).unwrap();
                assert_eq!(io.value.get(), Some(calibrated()));
            }
            ctm.set(true).unwrap();
        }
        assert_eq!(io.value.get(), Some(calibrated()));
    }
    #[test]
    fn nightlight_zero_blob_is_restored_as_zero_not_identity_blob() {
        let io = Fake::default();
        {
            let mut ctm = Ctm::new(io.clone()).unwrap();
            ctm.set(true).unwrap();
        }
        assert_eq!(io.value.get(), None);
    }
    #[test]
    fn nightlight_commit_rejection_and_readback_mismatch_roll_back() {
        for corrupt in [false, true] {
            let io = Fake::default();
            io.value.set(Some(calibrated()));
            let mut ctm = Ctm::new(io.clone()).unwrap();
            if corrupt {
                io.corrupt.set(true);
            } else {
                io.reject.set(1);
            }
            assert!(ctm.set(true).is_err());
            assert!(!ctm.verified().unwrap());
            assert_eq!(io.value.get(), Some(calibrated()));
        }
    }
    #[test]
    fn nightlight_disable_failure_keeps_actual_enabled_state() {
        let io = Fake::default();
        let mut ctm = Ctm::new(io.clone()).unwrap();
        ctm.set(true).unwrap();
        let warm = io.value.get();
        io.reject.set(1);
        assert!(ctm.set(false).is_err());
        assert!(ctm.verified().is_err());
        assert!(ctm.cleanup_pending());
        assert_eq!(io.value.get(), warm);
    }
    #[test]
    fn nightlight_external_calibration_is_never_overwritten() {
        let io = Fake::default();
        let mut ctm = Ctm::new(io.clone()).unwrap();
        ctm.set(true).unwrap();
        io.value.set(Some(calibrated()));
        io.owned.set(false);
        let count = io.writes.borrow().len();
        assert!(ctm.verified().is_err());
        assert!(ctm.set(false).is_err());
        drop(ctm);
        assert_eq!(io.writes.borrow().len(), count);
        assert_eq!(io.value.get(), Some(calibrated()));
    }
    #[test]
    fn nightlight_multi_output_atomic_policy_both_directions() {
        let a = Fake::default();
        let b = Fake::default();
        a.value.set(Some(calibrated()));
        let mut ca = Ctm::new(a.clone()).unwrap();
        let mut cb = Ctm::new(b.clone()).unwrap();
        b.reject.set(1);
        assert!(transaction(&mut [&mut ca, &mut cb], true).is_err());
        assert_eq!(a.value.get(), Some(calibrated()));
        assert_eq!(b.value.get(), None);
        transaction(&mut [&mut ca, &mut cb], true).unwrap();
        b.reject.set(1);
        assert!(transaction(&mut [&mut ca, &mut cb], false).is_err());
        assert!(!ca.verified().unwrap()); // Never re-warm A to hide failed B cleanup.
        assert!(cb.verified().is_err());
        let state = aggregate(
            vec![
                ca.output_status("A".into(), true),
                cb.output_status("B".into(), true),
            ],
            false,
            true,
            1,
        );
        assert_eq!(state.enabled, None);
        assert!(state.cleanup_pending && !state.error.is_empty());
        transaction(&mut [&mut ca, &mut cb], false).unwrap();
        assert_eq!(a.value.get(), Some(calibrated()));
        assert_eq!(b.value.get(), None);
    }
    #[test]
    fn failed_enable_readback_and_rollback_still_restore_on_drop() {
        let io = Fake::default();
        io.fail_read_at.borrow_mut().push(3); // initial capture, preflight, readback
        io.reject_at.borrow_mut().push(2); // rollback fails after successful warm write
        {
            let mut ctm = Ctm::new(io.clone()).unwrap();
            assert!(ctm.set(true).is_err());
            assert!(!ctm.enabled); // Was NEVER acknowledged enabled.
            assert!(ctm.dirty && ctm.cleanup_pending());
            assert!(ctm.verified().is_err());
            assert_ne!(io.value.get(), None);
        }
        assert_eq!(io.value.get(), None); // Destructor did not skip uncertain cleanup.
        assert_eq!(io.writes.borrow().len(), 3);
    }
    #[test]
    fn failed_multi_output_enable_and_rollback_preserve_independent_cleanup() {
        let a = Fake::default();
        let b = Fake::default();
        a.reject_at.borrow_mut().push(2); // A's rollback fails.
        b.reject_at.borrow_mut().push(1); // B rejects enable.
        {
            let mut ca = Ctm::new(a.clone()).unwrap();
            let mut cb = Ctm::new(b.clone()).unwrap();
            assert!(transaction(&mut [&mut ca, &mut cb], true).is_err());
            assert!(ca.cleanup_pending());
            let state = aggregate(
                vec![
                    ca.output_status("A".into(), true),
                    cb.output_status("B".into(), true),
                ],
                false,
                true,
                2,
            );
            assert_eq!(state.enabled, None);
            assert!(!state.error.is_empty());
            // Same independent cleanup used by disconnect/pre-disable/retry.
            ca.restore().unwrap();
            cb.restore().unwrap();
            let state = aggregate(
                vec![
                    ca.output_status("A".into(), true),
                    cb.output_status("B".into(), true),
                ],
                false,
                true,
                2,
            );
            assert_eq!(state.enabled, Some(false));
            assert!(!state.cleanup_pending);
        }
        assert_eq!(a.value.get(), None);
        assert_eq!(b.value.get(), None);
    }
    #[test]
    fn uncertain_cleanup_never_overwrites_external_blob() {
        let io = Fake::default();
        let mut ctm = Ctm::new(io.clone()).unwrap();
        ctm.set(true).unwrap();
        io.reject.set(1);
        assert!(ctm.restore().is_err());
        io.value.set(Some(calibrated()));
        io.owned.set(false);
        let writes = io.writes.borrow().len();
        assert!(ctm.restore().is_err());
        drop(ctm);
        assert_eq!(io.writes.borrow().len(), writes);
        assert_eq!(io.value.get(), Some(calibrated()));
    }
    #[test]
    fn pre_ack_restore_blocks_all_later_ctm_io_and_retains_failed_orphans() {
        for reject in [false, true] {
            let registry = Registry::<Fake>::new();
            registry.access.set(true);
            let io = Fake {
                access: Some(registry.access.clone()),
                ..Fake::default()
            };
            let surface = Rc::new(RefCell::new(Ctm::new(io.clone()).unwrap()));
            registry.entries.borrow_mut().push((
                1,
                crtc::Handle::from(std::num::NonZeroU32::new(1).unwrap()),
                "A".into(),
                surface.clone(),
            ));
            surface.borrow_mut().set(true).unwrap();
            if reject {
                io.reject.set(1);
            }
            registry.pre_disable(); // Actual hook, before backend ACK/revocation.
            assert!(!registry.access.get());
            assert_eq!(io.writes.borrow().len(), 2);
            if !reject {
                assert_eq!(io.value.get(), None);
            }
            // Nothing queued after the acknowledgement may write the device.
            assert!(surface.borrow_mut().set(true).is_err());
            registry.restore(false);
            assert_eq!(io.writes.borrow().len(), 2);
            drop(surface); // A disconnected paused surface does not lose cleanup.
            if reject {
                assert_eq!(registry.entries.borrow().len(), 1);
                assert!(registry.orphan_status()[0].cleanup_pending);
                registry.access.set(true); // Resume; safe restoration, not enable.
                registry.restore(true);
                assert_eq!(io.value.get(), None);
                assert!(registry.entries.borrow().is_empty());
            }
        }
    }
    #[test]
    fn reserved_owner_failed_enable_retries_cleanup_without_disconnect_or_hotplug() {
        let registry = Registry::<Fake>::new();
        registry.access.set(true);
        let io = Fake::default();
        io.fail_read_at.borrow_mut().push(3);
        io.reject_at.borrow_mut().push(2);
        let surface = Rc::new(RefCell::new(Ctm::new(io.clone()).unwrap()));
        registry.entries.borrow_mut().push((
            1,
            crtc::Handle::from(std::num::NonZeroU32::new(1).unwrap()),
            "A".into(),
            surface.clone(),
        ));
        let reserved_owner = Some(7_u64); // The connection stays alive throughout.
        assert!(surface.borrow_mut().set(true).is_err());
        assert_ne!(io.value.get(), None);
        let writes = io.writes.borrow().len();
        registry.retry_unacknowledged(false, false); // Never touch an inactive seat.
        registry.retry_unacknowledged(true, true); // Never undo acknowledged intent.
        assert_eq!(io.writes.borrow().len(), writes);
        registry.retry_unacknowledged(true, false); // The real timer's condition.
        assert_eq!(reserved_owner, Some(7));
        assert_eq!(io.value.get(), None);
        assert!(!surface.borrow().cleanup_pending());
        registry.retry_unacknowledged(true, false);
        assert_eq!(io.writes.borrow().len(), writes + 1); // No repeated write once clean.
    }
}
